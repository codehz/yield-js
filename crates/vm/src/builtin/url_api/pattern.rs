//! URLPattern stores normalized constructor input; the executable pattern is derived from it.

use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, define_builtin_data, link_constructor};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
use std::sync::{Arc, OnceLock};
use urlpattern::quirks::{self, StringOrInit};
use urlpattern::regexp::RegExp;
use urlpattern::{RegexSyntax, UrlPattern, UrlPatternInit, UrlPatternOptions};

const ROOT: &str = "urlPattern";
const PROTOTYPE: &str = "urlPattern.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("URLPattern", Slot::Global, "");
const TEST: IntrinsicKey = IntrinsicKey::new("URLPattern", Slot::Prototype, "test");
const EXEC: IntrinsicKey = IntrinsicKey::new("URLPattern", Slot::Prototype, "exec");

const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
        .fixed(PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(PROTOTYPE, RootKind::Plain, Some("object.prototype")),
];

macro_rules! getters {
    ($($name:literal => $get:expr),+ $(,)?) => {
        const GETTERS: &[IntrinsicDef] = &[$(
            IntrinsicDef::call(
                IntrinsicKey::new("URLPattern", Slot::PrototypeGetter, $name),
                0,
                |vm, _, roots, _, receiver| {
                    let Some(pattern) = receiver_pattern(vm, receiver)? else {
                        return vm.throw_type_error(roots, "URLPattern getter called on incompatible receiver");
                    };
                    Ok(Invocation::Value(($get)(&pattern)))
                },
            ),
        )+];
    };
}

getters! {
    "protocol" => |p: &Pattern| Value::string(p.protocol().into()),
    "username" => |p: &Pattern| Value::string(p.username().into()),
    "password" => |p: &Pattern| Value::string(p.password().into()),
    "hostname" => |p: &Pattern| Value::string(p.hostname().into()),
    "port" => |p: &Pattern| Value::string(p.port().into()),
    "pathname" => |p: &Pattern| Value::string(p.pathname().into()),
    "search" => |p: &Pattern| Value::string(p.search().into()),
    "hash" => |p: &Pattern| Value::string(p.hash().into()),
    "hasRegExpGroups" => |p: &Pattern| Value::boolean(p.has_regexp_groups()),
}

const INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        CONSTRUCTOR,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "URLPattern requires new"),
        construct,
    )
    .without_new_target_prototype(),
    IntrinsicDef::call(TEST, 0, |vm, host, roots, args, receiver| {
        matches(vm, host, roots, &args, receiver, false)
    }),
    IntrinsicDef::call(EXEC, 0, |vm, host, roots, args, receiver| {
        matches(vm, host, roots, &args, receiver, true)
    }),
];

pub(crate) const PATTERN_OWNER: Owner =
    Owner::new(ROOT, ROOTS, &[INTRINSICS, GETTERS], Some(install));

fn install(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    for def in GETTERS {
        let getter = vm.alloc_intrinsic(def.key);
        assert!(
            vm.heap
                .apply_descriptor(
                    prototype,
                    def.key.member.into(),
                    DescriptorUpdate {
                        get: Some(Some(getter)),
                        enumerable: Some(true),
                        configurable: Some(true),
                        ..DescriptorUpdate::default()
                    }
                )
                .expect("URLPattern prototype getter")
        );
    }
    for key in [TEST, EXEC] {
        let function = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            prototype,
            key.member,
            Value::object(function),
            Attributes {
                enumerable: true,
                ..Attributes::METHOD
            },
        );
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("URLPattern".into()),
        Attributes::FUNCTION_METADATA,
    );
}

// URLPattern regexes use ECMAScript syntax with Unicode Sets semantics. URLPattern
// 0.6 passes the legacy `u` flag to its adapter; replace it with the spec's `v`.
#[derive(Debug)]
struct PatternRegExp {
    source: String,
    regex: regress::Regex,
}

impl RegExp for PatternRegExp {
    fn syntax() -> RegexSyntax {
        RegexSyntax::EcmaScript
    }

    fn parse(pattern: &str, flags: &str, _: bool) -> Result<Self, ()> {
        let flags = regress::Flags {
            unicode: true,
            unicode_sets: true,
            icase: flags.contains('i'),
            ..regress::Flags::default()
        };
        Ok(Self {
            source: pattern.to_owned(),
            regex: regress::Regex::with_flags(pattern, flags).map_err(|_| ())?,
        })
    }

    fn matches<'a>(&self, text: &'a str) -> Option<Vec<Option<&'a str>>> {
        let matched = self.regex.find(text)?;
        Some(
            matched
                .captures
                .into_iter()
                .map(|range| range.map(|range| &text[range]))
                .collect(),
        )
    }

    fn pattern_string(&self) -> &str {
        &self.source
    }
}

type Pattern = UrlPattern<PatternRegExp>;

#[derive(Debug, Clone, Default)]
pub struct PatternCache(OnceLock<Result<Arc<Pattern>, String>>);

fn parse(init: UrlPatternInit, ignore_case: bool) -> Result<Pattern, urlpattern::Error> {
    Pattern::parse(
        init,
        UrlPatternOptions {
            regex_syntax: RegexSyntax::EcmaScript,
            ignore_case,
        },
    )
}

pub(crate) fn validate(input: &UrlPatternInit, ignore_case: bool) -> Result<(), String> {
    parse(input.clone(), ignore_case)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn receiver_pattern(vm: &VmCore, receiver: &Value) -> Result<Option<Arc<Pattern>>, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::UrlPattern {
            input,
            ignore_case,
            cache,
            ..
        } = vm.heap.get(id)?
    {
        return match cache.0.get_or_init(|| {
            parse(input.clone(), *ignore_case)
                .map(Arc::new)
                .map_err(|error| error.to_string())
        }) {
            Ok(parsed) => Ok(Some(Arc::clone(parsed))),
            Err(error) => Err(VmError::InvalidState(format!(
                "Invalid URLPattern: {error}"
            ))),
        };
    }
    Ok(None)
}

fn coerce_usv(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Result<Result<String, Value>, VmError> {
    Ok(vm.coerce_string(host, roots, value)?.map(|text| {
        text.to_utf8()
            .unwrap_or_else(|_| String::from_utf16_lossy(&text.to_utf16()))
    }))
}

fn convert_input(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Result<Result<StringOrInit<'static>, Value>, VmError> {
    match value.view() {
        ValueRef::Undefined | ValueRef::Null => {
            return Ok(Ok(StringOrInit::Init(Default::default())));
        }
        ValueRef::Object(_) => {}
        _ => return Ok(coerce_usv(vm, host, roots, value)?.map(|s| StringOrInit::String(s.into()))),
    }

    let mut init = quirks::UrlPatternInit::default();
    // Web IDL dictionary members are read in lexicographic order.
    for (name, slot) in [
        ("baseURL", &mut init.base_url),
        ("hash", &mut init.hash),
        ("hostname", &mut init.hostname),
        ("password", &mut init.password),
        ("pathname", &mut init.pathname),
        ("port", &mut init.port),
        ("protocol", &mut init.protocol),
        ("search", &mut init.search),
        ("username", &mut init.username),
    ] {
        let value = match vm.read_property(host, roots, value, PropertyKeyRef::String(name))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if !matches!(value.view(), ValueRef::Undefined) {
            *slot = match coerce_usv(vm, host, &roots.with(&value), &value)? {
                Ok(value) => Some(value),
                Err(error) => return Ok(Err(error)),
            };
        }
    }
    Ok(Ok(StringOrInit::Init(init)))
}

fn normalize_constructor_input(
    input: StringOrInit<'_>,
    base: Option<&str>,
) -> Result<UrlPatternInit, urlpattern::Error> {
    if let StringOrInit::String(pattern) = input {
        let base = base
            .map(url::Url::parse)
            .transpose()
            .map_err(urlpattern::Error::Url)?;
        return UrlPatternInit::parse_constructor_string::<PatternRegExp>(&pattern, base);
    }
    quirks::process_construct_pattern_input(input, base)
}

fn construct(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let input = match convert_input(vm, host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
        Ok(input) => input,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let undefined = Value::UNDEFINED;
    let second = args.get(1).unwrap_or(&undefined);
    let (base, options) = if args.len() >= 3
        || !matches!(
            second.view(),
            ValueRef::Object(_) | ValueRef::Null | ValueRef::Undefined
        ) {
        let base = match coerce_usv(vm, host, roots, second)? {
            Ok(base) => base,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        (Some(base), args.get(2).unwrap_or(&undefined))
    } else {
        (None, second)
    };
    let ignore_case = match options.view() {
        ValueRef::Undefined | ValueRef::Null => false,
        ValueRef::Object(_) => {
            match vm.read_property(host, roots, options, PropertyKeyRef::String("ignoreCase"))? {
                Invocation::Value(value) => value.truthy(),
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        _ => return vm.throw_type_error(roots, "URLPattern options must be an object"),
    };
    let input = match normalize_constructor_input(input, base.as_deref()) {
        Ok(input) => input,
        Err(error) => return vm.throw_type_error(roots, &format!("Invalid URLPattern: {error}")),
    };
    let parsed = match parse(input.clone(), ignore_case) {
        Ok(parsed) => parsed,
        Err(error) => return vm.throw_type_error(roots, &format!("Invalid URLPattern: {error}")),
    };
    let prototype =
        match vm.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => id,
                _ => vm.builtins.get(PROTOTYPE),
            },
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::UrlPattern {
            object: PlainObject::new(Some(prototype)),
            input,
            ignore_case,
            cache: PatternCache(OnceLock::from(Ok(Arc::new(parsed)))),
        },
    ))))
}

fn matches(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    receiver: &Value,
    exec: bool,
) -> Result<Invocation, VmError> {
    let Some(pattern) = receiver_pattern(vm, receiver)? else {
        return vm.throw_type_error(roots, "URLPattern method called on incompatible receiver");
    };
    let roots = &roots.with(&args);
    let input = match convert_input(vm, host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
        Ok(input) => input,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let base = match args
        .get(1)
        .filter(|arg| !matches!(arg.view(), ValueRef::Undefined))
    {
        Some(value) => match coerce_usv(vm, host, roots, value)? {
            Ok(base) => Some(base),
            Err(error) => return Ok(Invocation::Thrown(error)),
        },
        None => None,
    };
    let no_match = || {
        Invocation::Value(if exec {
            Value::NULL
        } else {
            Value::boolean(false)
        })
    };
    let (input, inputs) = match quirks::process_match_input(input, base.as_deref()) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return Ok(no_match()),
        Err(error) => return vm.throw_type_error(roots, &error.to_string()),
    };
    let Some(input) = quirks::parse_match_input(input) else {
        return Ok(no_match());
    };
    let components = [
        ("protocol", &pattern.protocol, input.protocol),
        ("username", &pattern.username, input.username),
        ("password", &pattern.password, input.password),
        ("hostname", &pattern.hostname, input.hostname),
        ("port", &pattern.port, input.port),
        ("pathname", &pattern.pathname, input.pathname),
        ("search", &pattern.search, input.search),
        ("hash", &pattern.hash, input.hash),
    ];
    // The dependency's optimized matchers strip literal prefixes and suffixes,
    // which defeats ignoreCase and hides text from assertions such as lookbehind.
    let mut captures = [const { Vec::new() }; 8];
    for ((_, pattern, input), captures) in components.iter().zip(&mut captures) {
        let Some(matched) = pattern
            .regexp
            .as_ref()
            .ok()
            .and_then(|re| re.regex.find(input))
        else {
            return Ok(no_match());
        };
        if exec {
            *captures = matched.captures;
        }
    }
    if !exec {
        return Ok(Invocation::Value(Value::boolean(true)));
    }
    let (input, base) = inputs;
    let input = match input {
        StringOrInit::String(value) => Value::string(value.as_ref().into()),
        StringOrInit::Init(init) => {
            let object = vm.heap.alloc_plain();
            for (name, value) in [
                ("baseURL", init.base_url),
                ("hash", init.hash),
                ("hostname", init.hostname),
                ("password", init.password),
                ("pathname", init.pathname),
                ("port", init.port),
                ("protocol", init.protocol),
                ("search", init.search),
                ("username", init.username),
            ] {
                if let Some(value) = value {
                    vm.heap
                        .set_own_string(object, name, Value::string(value.into()))?;
                }
            }
            Value::object(object)
        }
    };
    let mut inputs = vec![input];
    if let Some(base) = base {
        inputs.push(Value::string(base.into()));
    }
    let inputs = vm.heap.alloc_array(inputs);
    let object = vm.heap.alloc_plain();
    vm.heap
        .set_own_string(object, "inputs", Value::object(inputs))?;
    for ((name, pattern, input), captures) in components.into_iter().zip(captures) {
        let component = vm.heap.alloc_plain();
        let groups = vm.heap.alloc_plain();
        for (index, name) in pattern.group_name_list.iter().enumerate() {
            let value = captures
                .get(index)
                .and_then(Option::as_ref)
                .map_or(Value::UNDEFINED, |range| {
                    Value::string(input[range.clone()].into())
                });
            vm.heap.set_own_string(groups, name, value)?;
        }
        vm.heap
            .set_own_string(component, "input", Value::string(input.into()))?;
        vm.heap
            .set_own_string(component, "groups", Value::object(groups))?;
        vm.heap
            .set_own_string(object, name, Value::object(component))?;
    }
    Ok(Invocation::Value(Value::object(object)))
}
