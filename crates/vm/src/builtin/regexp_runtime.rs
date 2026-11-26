//! RegExp objects and matching over JavaScript UTF-16 code units.
use std::borrow::Cow;

use crate::builtin::globals::Owner;
use crate::builtin::{RootKind, RootSpec, define_builtin_data, link_constructor};
use crate::error::{ErrorKind, VmError};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ITERATOR_SYMBOL, Object, PlainObject};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::regexp::{compile_pattern, encode_input, encode_input_with_flags, find_match};
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Clone, Copy)]
pub(crate) enum RegExpMethod {
    Exec,
    Test,
    ToString,
    Source,
    Flags,
    Global,
    IgnoreCase,
    Multiline,
    DotAll,
    Unicode,
    HasIndices,
    UnicodeSets,
    Sticky,
    Match,
    MatchAll,
    Replace,
    Search,
    Split,
    Escape,
}

const REGEXP: IntrinsicKey = IntrinsicKey::new(members::NAME, Slot::Global, "");
const ITERATOR_NEXT: IntrinsicKey =
    IntrinsicKey::new("RegExpStringIterator", Slot::Prototype, "next");

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = Owner::new(
        members::ROOT,
        REGEXP_ROOTS,
        &[
            &[IntrinsicDef::constructor(
                REGEXP,
                2,
                |vm, host, roots, args, _| vm.construct_regexp(host, roots, &args, None, true),
                |vm, host, roots, args, prototype| {
                    vm.construct_regexp(host, roots, &args, prototype, false)
                },
            )],
            members::Globals::<P>::INTRINSICS,
            &[IntrinsicDef::call(
                ITERATOR_NEXT,
                0,
                |vm, host, roots, _, receiver| {
                    vm.regexp_string_iterator_next(host, roots, receiver)
                },
            )],
        ],
        Some(install),
    );
}

const REGEXP_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        members::ROOT,
        RootKind::Native(REGEXP),
        Some("function.prototype"),
    )
    .fixed("regexp.prototype"),
    RootSpec::new(
        "regexp.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
    RootSpec::new(
        "regexpIterator.prototype",
        RootKind::Plain,
        Some(super::iterator::SYNC_ROOT),
    ),
];

#[yield_binding_macros::builtin_global(name = "RegExp", root = "regexp", kind = "constructor")]
mod members {
    use super::RegExpMethod::*;
    use crate::heap::{
        MATCH_ALL_SYMBOL, MATCH_SYMBOL, REPLACE_SYMBOL, SEARCH_SYMBOL, SPECIES_SYMBOL, SPLIT_SYMBOL,
    };
    use crate::runtime::Invocation;

    prototype_calls! {
        exec: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Exec, receiver, &args);
        test: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Test, receiver, &args);
        toString: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, ToString, receiver, &args);
        get source: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Source, receiver, &args);
        get flags: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Flags, receiver, &args);
        get global: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Global, receiver, &args);
        get ignoreCase: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, IgnoreCase, receiver, &args);
        get multiline: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Multiline, receiver, &args);
        get dotAll: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, DotAll, receiver, &args);
        get unicode: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Unicode, receiver, &args);
        get hasIndices: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, HasIndices, receiver, &args);
        get unicodeSets: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, UnicodeSets, receiver, &args);
        get sticky: 0 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Sticky, receiver, &args);
    }

    static_calls! {
        escape: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Escape, receiver, &args);
        get symbol(SPECIES_SYMBOL) species: 0 => |_, _, _, _, receiver| {
            Ok(Invocation::Value(receiver.clone()))
        };
    }

    prototype_calls! {
        symbol(MATCH_SYMBOL) r#match: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Match, receiver, &args);
        symbol(MATCH_ALL_SYMBOL) matchAll: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, MatchAll, receiver, &args);
        symbol(REPLACE_SYMBOL) replace: 2 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Replace, receiver, &args);
        symbol(SEARCH_SYMBOL) search: 1 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Search, receiver, &args);
        symbol(SPLIT_SYMBOL) split: 2 => |vm, host, roots, args, receiver| vm.call_regexp_method(host, roots, Split, receiver, &args);
    }
}

fn install(vm: &mut VmCore, identity: &Value) {
    let constructor = vm.builtins.get(members::ROOT);
    let prototype = vm.builtins.get("regexp.prototype");
    let iterator = vm.builtins.get("regexpIterator.prototype");
    link_constructor(vm, constructor, prototype);
    members::install_members(vm, constructor, prototype);
    let next = vm.alloc_intrinsic(ITERATOR_NEXT);
    define_builtin_data(
        vm,
        iterator,
        ITERATOR_NEXT.member,
        Value::object(next),
        Attributes::METHOD,
    );
    define_builtin_data(
        vm,
        iterator,
        PropertyKey::Symbol(ITERATOR_SYMBOL),
        identity.clone(),
        Attributes::METHOD,
    );
}

impl VmCore {
    /// A literal was validated by the compiler, but bytecode may come from elsewhere.
    pub(crate) fn alloc_regexp(
        &mut self,
        source: &JsString,
        flags: &str,
    ) -> Result<ObjectId, VmError> {
        let (flags, compiled) = compile_pattern(source, flags).map_err(VmError::InvalidBuild)?;
        self.alloc_compiled_regexp(
            source.to_owned(),
            flags,
            compiled,
            self.builtins.get("regexp.prototype"),
        )
    }

    pub(super) fn alloc_compiled_regexp(
        &mut self,
        source: JsString,
        flags: crate::regexp::RegExpFlags,
        compiled: regress::Regex,
        prototype: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let id = self.heap.alloc(Object::RegExp {
            source,
            flags,
            compiled,
            object: PlainObject::new(Some(prototype)),
        });
        self.heap.define_data(
            id,
            "lastIndex",
            Value::number(0.0),
            Attributes::FUNCTION_PROTOTYPE,
        )?;
        Ok(id)
    }

    pub(crate) fn construct_regexp<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
        called: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let undefined = Value::UNDEFINED;
        let pattern = args.first().unwrap_or(&undefined);
        let flags_arg = args.get(1).unwrap_or(&undefined);
        let regexp = match pattern.view() {
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::RegExp { .. } => Some(id),
                _ => None,
            },
            _ => None,
        };
        if called && regexp.is_some() && matches!(flags_arg.view(), ValueRef::Undefined) {
            match self.read_property(host, roots, pattern, PropertyKeyRef::String("constructor"))? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Object(id) if id == self.builtins.get("regexp")) =>
                {
                    return Ok(Invocation::Value(pattern.clone()));
                }
                Invocation::Value(_) => {}
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        let (source, inherited_flags) = match regexp {
            Some(id) => match self.heap.get(id)? {
                Object::RegExp { source, flags, .. } => (source.clone(), Some(*flags)),
                _ => unreachable!("RegExp object kind does not change"),
            },
            None => {
                let source = if matches!(pattern.view(), ValueRef::Undefined) {
                    JsString::new()
                } else {
                    match self.coerce_string(host, roots, pattern)? {
                        Ok(source) => source,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                (source, None)
            }
        };
        let flags_text = if matches!(flags_arg.view(), ValueRef::Undefined) {
            inherited_flags.map_or_else(JsString::new, |flags| flags.as_string().into())
        } else {
            match self.coerce_string(host, roots, flags_arg)? {
                Ok(flags) => flags,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let compiled = flags_text
            .to_utf8()
            .map_err(|_| "invalid RegExp flags".to_owned())
            .and_then(|flags| compile_pattern(&source, &flags));
        let (flags, compiled) = match compiled {
            Ok(compiled) => compiled,
            Err(message) => {
                return self
                    .alloc_class_error(roots, ErrorKind::SyntaxError.into(), Some(message.into()))
                    .map(Invocation::Thrown);
            }
        };
        let prototype = prototype.unwrap_or_else(|| self.builtins.get("regexp.prototype"));
        self.alloc_compiled_regexp(source, flags, compiled, prototype)
            .map(|id| Invocation::Value(Value::object(id)))
    }

    pub(crate) fn call_regexp_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: RegExpMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        if matches!(method, RegExpMethod::Escape) {
            let Some(ValueRef::String(input)) = args.first().map(Value::view) else {
                return self.throw_type_error(roots, "RegExp.escape requires a string argument");
            };
            return Ok(Invocation::Value(Value::string(regexp_escape(input))));
        }
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "RegExp method requires an object receiver");
        };
        match method {
            RegExpMethod::ToString => {
                let source = match self.regexp_string_property(host, roots, receiver, "source")? {
                    Ok(source) => source,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let flags = match self.regexp_string_property(host, roots, receiver, "flags")? {
                    Ok(flags) => flags,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let mut text = JsString::from("/");
                text.push_str(&source);
                text.push('/');
                text.push_str(&flags);
                return Ok(Invocation::Value(Value::string(text)));
            }
            RegExpMethod::Flags => {
                let mut flags = String::new();
                for (key, flag) in [
                    ("hasIndices", 'd'),
                    ("global", 'g'),
                    ("ignoreCase", 'i'),
                    ("multiline", 'm'),
                    ("dotAll", 's'),
                    ("unicode", 'u'),
                    ("unicodeSets", 'v'),
                    ("sticky", 'y'),
                ] {
                    match self.read_property(host, roots, receiver, PropertyKeyRef::String(key))? {
                        Invocation::Value(value) => {
                            if value.truthy() {
                                flags.push(flag);
                            }
                        }
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    }
                }
                return Ok(Invocation::Value(Value::string(flags.into())));
            }
            RegExpMethod::Test => {
                return self.regexp_test(
                    host,
                    roots,
                    receiver,
                    id,
                    args.first().unwrap_or(&Value::UNDEFINED),
                );
            }
            RegExpMethod::Match => return self.regexp_symbol_match(host, roots, id, args),
            RegExpMethod::MatchAll => return self.regexp_symbol_match_all(host, roots, id, args),
            RegExpMethod::Replace => return self.regexp_symbol_replace(host, roots, id, args),
            RegExpMethod::Search => return self.regexp_symbol_search(host, roots, id, args),
            RegExpMethod::Split => return self.regexp_symbol_split(host, roots, id, args),
            _ => {}
        }
        let Object::RegExp { source, flags, .. } = self.heap.get(id)? else {
            if id == self.builtins.get("regexp.prototype") {
                match method {
                    RegExpMethod::Source => {
                        return Ok(Invocation::Value(Value::string("(?:)".into())));
                    }
                    RegExpMethod::Exec => {}
                    _ => return Ok(Invocation::Value(Value::UNDEFINED)),
                }
            }
            return self.throw_type_error(roots, "RegExp method requires a RegExp receiver");
        };
        let value = match method {
            RegExpMethod::Exec => {
                let undefined = Value::UNDEFINED;
                let argument = args.first().unwrap_or(&undefined);
                let input = match self.coerce_string(host, roots, argument)? {
                    Ok(input) => input,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                return self.regexp_match(host, roots, id, input);
            }
            RegExpMethod::Source => Value::string(escaped_source(source)),
            RegExpMethod::Global => Value::boolean(flags.global),
            RegExpMethod::IgnoreCase => Value::boolean(flags.ignore_case),
            RegExpMethod::Multiline => Value::boolean(flags.multiline),
            RegExpMethod::DotAll => Value::boolean(flags.dot_all),
            RegExpMethod::HasIndices => Value::boolean(flags.has_indices),
            RegExpMethod::Sticky => Value::boolean(flags.sticky),
            RegExpMethod::Unicode => Value::boolean(flags.unicode),
            RegExpMethod::UnicodeSets => Value::boolean(flags.unicode_sets),
            RegExpMethod::Match
            | RegExpMethod::MatchAll
            | RegExpMethod::Replace
            | RegExpMethod::Search
            | RegExpMethod::Split
            | RegExpMethod::Test
            | RegExpMethod::ToString
            | RegExpMethod::Flags
            | RegExpMethod::Escape => unreachable!(),
        };
        Ok(Invocation::Value(value))
    }
    fn regexp_symbol_match<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let receiver = Value::object(id);
        let string =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let flags = match self.regexp_flags(host, roots, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if !flags_contains(&flags, b'g') {
            return self.regexp_exec(host, roots, &receiver, id, string);
        }
        if let Err(error) = self.regexp_set_last_index(host, roots, id, 0.0)? {
            return Ok(Invocation::Thrown(error));
        }
        let unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let encoded = encode_input_with_flags(&string, &flags).into_owned();
        let mut results = Vec::new();
        loop {
            match self.regexp_exec_with_encoded(
                host,
                roots,
                &receiver,
                id,
                string.clone(),
                Some(encoded.as_ref()),
            )? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Null) => break,
                Invocation::Value(result) => {
                    let ValueRef::Object(array) = result.view() else {
                        return self
                            .throw_type_error(roots, "RegExp exec must return an object or null");
                    };
                    let matched = match self.regexp_capture(host, roots, array, 0)? {
                        Ok(value) => match self.coerce_string(host, roots, &value)? {
                            Ok(matched) => matched,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        },
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                    results.push(Value::string(matched.clone()));
                    if matched.is_empty() {
                        let last_index = match self.regexp_last_index(host, roots, id)? {
                            Ok(index) => index,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        let next = advance_string_index(&string, last_index, unicode);
                        if let Err(error) = self.regexp_set_last_index(host, roots, id, next)? {
                            return Ok(Invocation::Thrown(error));
                        }
                    }
                }
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        if results.is_empty() {
            return Ok(Invocation::Value(Value::NULL));
        }
        self.array_result(results)
    }

    fn regexp_symbol_match_all<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let receiver = Value::object(id);
        let string =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let constructor = match self.species_constructor(host, roots, &receiver, members::ROOT)? {
            Ok(constructor) => constructor,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&constructor);
        let flags = match self.regexp_flags(host, roots, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let global = flags_contains(&flags, b'g');
        let full_unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let created = self.construct_with(
            host,
            roots,
            &constructor,
            Arguments::Values(&[receiver, Value::string(flags)]),
            &constructor,
        )?;
        let Invocation::Value(value) = &created else {
            return Ok(created);
        };
        let ValueRef::Object(matcher) = value.view() else {
            return Ok(created);
        };
        let roots = &roots.with(&matcher);
        let last_index = match self.regexp_last_index(host, roots, id)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if let Err(error) =
            self.regexp_set_last_index(host, roots, matcher, to_length(last_index))?
        {
            return Ok(Invocation::Thrown(error));
        }
        let iterator = self.heap.alloc(Object::RegExpStringIterator {
            object: PlainObject::new(Some(self.builtins.get("regexpIterator.prototype"))),
            regexp: matcher,
            string,
            global,
            full_unicode,
            done: false,
        });
        Ok(Invocation::Value(Value::object(iterator)))
    }

    fn regexp_string_iterator_next<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        let invalid = "RegExpStringIterator.next requires a RegExp string iterator";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid);
        };
        let Object::RegExpStringIterator {
            regexp,
            string,
            global,
            full_unicode,
            done,
            ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(roots, invalid);
        };
        let (regexp, string, global, full_unicode, done) =
            (*regexp, string.clone(), *global, *full_unicode, *done);
        if done {
            return self.iterator_result(Value::UNDEFINED, true);
        }
        let matcher = Value::object(regexp);
        let execution = self.regexp_exec(host, roots, &matcher, regexp, string.clone())?;
        match execution {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Null) => {
                let Object::RegExpStringIterator { done, .. } = self.heap.get_mut(id)? else {
                    unreachable!("iterator kind does not change");
                };
                *done = true;
                self.iterator_result(Value::UNDEFINED, true)
            }
            Invocation::Value(result) => {
                let ValueRef::Object(array) = result.view() else {
                    return self
                        .throw_type_error(roots, "RegExp exec must return an object or null");
                };
                let roots = &roots.with(&array);
                if global {
                    let matched = match self.regexp_capture(host, roots, array, 0)? {
                        Ok(value) => match self.coerce_string(host, roots, &value)? {
                            Ok(matched) => matched,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        },
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                    if matched.is_empty() {
                        let last_index = match self.regexp_last_index(host, roots, regexp)? {
                            Ok(index) => index,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        let next = advance_string_index(&string, last_index, full_unicode);
                        if let Err(error) = self.regexp_set_last_index(host, roots, regexp, next)? {
                            return Ok(Invocation::Thrown(error));
                        }
                    }
                } else {
                    let Object::RegExpStringIterator { done, .. } = self.heap.get_mut(id)? else {
                        unreachable!("iterator kind does not change");
                    };
                    *done = true;
                }
                self.iterator_result(result, false)
            }
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }

    fn regexp_symbol_replace<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let receiver = Value::object(id);
        let string =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let replace_value = args.get(1).cloned().unwrap_or(Value::UNDEFINED);
        let functional = self.is_callable(&replace_value);
        let replacement = if functional {
            JsString::default()
        } else {
            match self.coerce_string(host, roots, &replace_value)? {
                Ok(replacement) => replacement,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let flags = match self.regexp_flags(host, roots, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let global = flags_contains(&flags, b'g');
        let unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let mut results = Vec::new();
        if global {
            if let Err(error) = self.regexp_set_last_index(host, roots, id, 0.0)? {
                return Ok(Invocation::Thrown(error));
            }
            let encoded = encode_input_with_flags(&string, &flags).into_owned();
            loop {
                match self.regexp_exec_with_encoded(
                    host,
                    &roots.with(&results),
                    &receiver,
                    id,
                    string.clone(),
                    Some(encoded.as_ref()),
                )? {
                    Invocation::Value(result) if matches!(result.view(), ValueRef::Null) => break,
                    Invocation::Value(result) => {
                        if !matches!(result.view(), ValueRef::Object(_)) {
                            return self.throw_type_error(
                                roots,
                                "RegExp exec must return an object or null",
                            );
                        }
                        results.push(result);
                        let roots = &roots.with(&results);
                        let array = match results.last().expect("just pushed").view() {
                            ValueRef::Object(array) => array,
                            _ => unreachable!("validated regexp result"),
                        };
                        let matched = match self.regexp_capture(host, roots, array, 0)? {
                            Ok(value) => match self.coerce_string(host, roots, &value)? {
                                Ok(matched) => matched,
                                Err(error) => return Ok(Invocation::Thrown(error)),
                            },
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        if matched.is_empty() {
                            let last_index = match self.regexp_last_index(host, roots, id)? {
                                Ok(index) => index,
                                Err(error) => return Ok(Invocation::Thrown(error)),
                            };
                            let next = advance_string_index(&string, last_index, unicode);
                            if let Err(error) = self.regexp_set_last_index(host, roots, id, next)? {
                                return Ok(Invocation::Thrown(error));
                            }
                        }
                    }
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            }
        } else {
            match self.regexp_exec(host, roots, &receiver, id, string.clone())? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Null) => {
                    return Ok(Invocation::Value(Value::string(string)));
                }
                Invocation::Value(result) => {
                    if !matches!(result.view(), ValueRef::Object(_)) {
                        return self
                            .throw_type_error(roots, "RegExp exec must return an object or null");
                    }
                    results.push(result);
                }
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        if results.is_empty() {
            return Ok(Invocation::Value(Value::string(string)));
        }
        match self.regexp_replace_results(
            host,
            roots,
            &string,
            results,
            functional,
            &replace_value,
            &replacement,
        )? {
            Ok(out) => Ok(Invocation::Value(Value::string(out))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn regexp_symbol_search<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let receiver = Value::object(id);
        let string =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let previous = match self.regexp_last_index(host, roots, id)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if previous.to_bits() != 0.0f64.to_bits()
            && let Err(error) = self.regexp_set_last_index(host, roots, id, 0.0)?
        {
            return Ok(Invocation::Thrown(error));
        }
        let result = self.regexp_exec(host, roots, &receiver, id, string)?;
        let roots = &roots.with(&result);
        let current = match self.regexp_last_index(host, roots, id)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if current.to_bits() != previous.to_bits()
            && let Err(error) = self.regexp_set_last_index(host, roots, id, previous)?
        {
            return Ok(Invocation::Thrown(error));
        }
        match &result {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Null) => {
                Ok(Invocation::Value(Value::number(-1.0)))
            }
            Invocation::Value(result) => {
                let ValueRef::Object(array) = result.view() else {
                    return self
                        .throw_type_error(roots, "RegExp exec must return an object or null");
                };
                match self.regexp_element(host, roots, array, "index")? {
                    Ok(value) => match self.coerce_number(host, roots, &value)? {
                        Ok(number) => Ok(Invocation::Value(Value::number(to_integer(number)))),
                        Err(error) => Ok(Invocation::Thrown(error)),
                    },
                    Err(error) => Ok(Invocation::Thrown(error)),
                }
            }
            Invocation::Thrown(_) => Ok(result),
        }
    }

    fn regexp_symbol_split<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let receiver = Value::object(id);
        let string =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let constructor = match self.species_constructor(host, roots, &receiver, members::ROOT)? {
            Ok(constructor) => constructor,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&constructor);
        let mut splitter_flags = match self.regexp_flags(host, roots, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let unicode =
            flags_contains(&splitter_flags, b'u') || flags_contains(&splitter_flags, b'v');
        if !flags_contains(&splitter_flags, b'y') {
            splitter_flags.push('y');
        }
        let created = self.construct_with(
            host,
            roots,
            &constructor,
            Arguments::Values(&[receiver, Value::string(splitter_flags)]),
            &constructor,
        )?;
        let Invocation::Value(value) = &created else {
            return Ok(created);
        };
        let ValueRef::Object(splitter) = value.view() else {
            return Ok(created);
        };
        let roots = &roots.with(&splitter);
        let limit = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.coerce_number(host, roots, value)? {
                    Ok(number) => crate::ops::number_to_uint32(number) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => u32::MAX as usize,
        };
        let size = string.unit_len();
        let mut parts = Vec::new();
        if limit == 0 {
            return self.array_result(parts);
        }
        // A species constructor can return a RegExp with entirely different flags.
        let encoded = match self.heap.get(splitter)? {
            Object::RegExp { flags, .. } => Some(encode_input(&string, *flags)),
            _ => None,
        };
        if size == 0 {
            match self.regexp_exec_with_encoded(
                host,
                roots,
                &Value::object(splitter),
                splitter,
                string.clone(),
                encoded.as_deref(),
            )? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Null) => {
                    parts.push(Value::string(string))
                }
                Invocation::Value(_) => {}
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
            return self.array_result(parts);
        }
        let mut start = 0usize;
        let mut cursor = 0usize;
        while cursor < size {
            let step_roots = &roots.with(&parts);
            if let Err(error) =
                self.regexp_set_last_index(host, step_roots, splitter, cursor as f64)?
            {
                return Ok(Invocation::Thrown(error));
            }
            let result = self.regexp_exec_with_encoded(
                host,
                step_roots,
                &Value::object(splitter),
                splitter,
                string.clone(),
                encoded.as_deref(),
            )?;
            let Invocation::Value(result) = result else {
                return Ok(result);
            };
            let ValueRef::Object(array) = result.view() else {
                cursor = advance_string_index(&string, cursor as f64, unicode) as usize;
                continue;
            };
            let roots = &roots.with(&array);
            let end = match self.regexp_last_index(host, &roots.with(&parts), splitter)? {
                Ok(index) => (to_length(index) as usize).min(size),
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if end == start {
                cursor = advance_string_index(&string, cursor as f64, unicode) as usize;
                continue;
            }
            parts.push(Value::string(string.slice_units(start.min(cursor)..cursor)));
            if parts.len() == limit {
                return self.array_result(parts);
            }
            start = end;
            cursor = end;
            let count = match self.regexp_element(host, &roots.with(&parts), array, "length")? {
                Ok(value) => match self.coerce_number(host, &roots.with(&parts), &value)? {
                    Ok(number) => to_length(number) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            for index in 1..count {
                let value = match self.regexp_capture(host, &roots.with(&parts), array, index)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                parts.push(value);
                if parts.len() == limit {
                    return self.array_result(parts);
                }
            }
        }
        parts.push(Value::string(string.slice_units(start..size)));
        self.array_result(parts)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    fn regexp_replace_results<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        string: &JsString,
        results: Vec<Value>,
        functional: bool,
        replace_value: &Value,
        replacement: &JsString,
    ) -> Result<Result<JsString, Value>, VmError> {
        let length = string.unit_len();
        let mut out = Vec::new();
        let mut next = 0usize;
        let root_view = (&results, replace_value);
        let roots = &roots.with(&root_view);
        for result in &results {
            let ValueRef::Object(array) = result.view() else {
                continue;
            };
            let roots = &roots.with(&array);
            let matched_value = match self.regexp_element(host, roots, array, "0")? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            let matched = match self.coerce_string(host, roots, &matched_value)? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            let index = match self.regexp_element(host, roots, array, "index")? {
                Ok(value) => match self.coerce_number(host, roots, &value)? {
                    Ok(number) => to_integer(number),
                    Err(error) => return Ok(Err(error)),
                },
                Err(error) => return Ok(Err(error)),
            };
            let position = if index.is_nan() || index < 0.0 {
                0
            } else if index > length as f64 {
                length
            } else {
                index as usize
            };
            let tail = (position + matched.unit_len()).min(length);
            let count = match self.regexp_element(host, roots, array, "length")? {
                Ok(value) => match self.coerce_number(host, roots, &value)? {
                    Ok(number) => to_integer(number).clamp(0.0, u32::MAX as f64) as usize,
                    Err(error) => return Ok(Err(error)),
                },
                Err(error) => return Ok(Err(error)),
            };
            let mut captures = Vec::new();
            for index in 1..count {
                match self.regexp_capture(host, &roots.with(&captures), array, index)? {
                    Ok(value) => captures.push(value),
                    Err(error) => return Ok(Err(error)),
                }
            }
            let groups = match self.regexp_element(host, &roots.with(&captures), array, "groups")? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            let start = next.min(position);
            out.extend(string.units().skip(start).take(position - start));
            if functional {
                let mut argv = Vec::with_capacity(captures.len() + 4);
                argv.push(Value::string(matched.clone()));
                argv.extend(captures.iter().cloned());
                argv.push(Value::number(position as f64));
                argv.push(Value::string(string.clone()));
                if !matches!(groups.view(), ValueRef::Undefined) {
                    argv.push(groups.clone());
                }
                let value = match self.invoke(
                    host,
                    roots,
                    replace_value,
                    Arguments::Values(&argv),
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => match thrown {
                        Invocation::Thrown(error) => return Ok(Err(error)),
                        _ => unreachable!(),
                    },
                };
                match self.coerce_string(host, roots, &value)? {
                    Ok(value) => out.extend(value.units()),
                    Err(error) => return Ok(Err(error)),
                }
            } else {
                let view = (&captures, &groups);
                let roots = &roots.with(&view);
                let parts = Substitution {
                    string,
                    matched: &matched,
                    position,
                    captures: &captures,
                    named: &groups,
                };
                match self.regexp_substitution(host, roots, replacement, &parts)? {
                    Ok(value) => out.extend(value.units()),
                    Err(error) => return Ok(Err(error)),
                }
            }
            next = tail;
        }
        out.extend(string.units().skip(next.min(length)));
        Ok(Ok(JsString::from_units(out)))
    }

    fn regexp_substitution<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        replaced: &JsString,
        parts: &Substitution<'_>,
    ) -> Result<Result<JsString, Value>, VmError> {
        let Substitution {
            string,
            matched,
            position,
            captures,
            named,
        } = *parts;
        let mut units = replaced.units().enumerate().peekable();
        let mut last_close = None;
        let length = string.unit_len();
        let tail = (position + matched.unit_len()).min(length);
        let mut out = Vec::new();
        while let Some((index, unit)) = units.next() {
            if unit != u16::from(b'$') {
                out.push(unit);
                continue;
            }
            match units.peek().map(|&(_, unit)| unit as u8) {
                Some(b'$') => {
                    units.next();
                    out.push(u16::from(b'$'));
                }
                Some(b'&') => {
                    units.next();
                    out.extend(matched.units());
                }
                Some(b'`') => {
                    units.next();
                    out.extend(string.units().take(position.min(length)));
                }
                Some(b'\'') => {
                    units.next();
                    out.extend(string.units().skip(tail));
                }
                Some(digit @ b'0'..=b'9') => {
                    let (_, digit_unit) = units.next().expect("peeked a capture digit");
                    let mut number = usize::from(digit - b'0');
                    if let Some((_, next)) = units.peek()
                        && (0x30..=0x39).contains(next)
                    {
                        let two = number * 10 + usize::from(*next - 0x30);
                        if two >= 1 && two <= captures.len() {
                            number = two;
                            units.next();
                        }
                    }
                    if number >= 1 && number <= captures.len() {
                        if !matches!(captures[number - 1].view(), ValueRef::Undefined) {
                            match self.coerce_string(host, roots, &captures[number - 1])? {
                                Ok(value) => out.extend(value.units()),
                                Err(error) => return Ok(Err(error)),
                            }
                        }
                    } else {
                        out.extend([unit, digit_unit]);
                    }
                }
                Some(b'<') => {
                    let close = *last_close.get_or_insert_with(|| {
                        replaced.units().rposition(|unit| unit == u16::from(b'>'))
                    });
                    if !close.is_some_and(|close| close > index + 1) {
                        out.push(unit);
                        continue;
                    }
                    let (_, open) = units.next().expect("peeked opening delimiter");
                    if matches!(named.view(), ValueRef::Undefined) {
                        out.extend([unit, open]);
                        for (_, unit) in units.by_ref() {
                            out.push(unit);
                            if unit == u16::from(b'>') {
                                break;
                            }
                        }
                        continue;
                    }
                    let key = JsString::from_units(
                        units
                            .by_ref()
                            .map(|(_, unit)| unit)
                            .take_while(|&unit| unit != u16::from(b'>'))
                            .collect(),
                    );
                    let value = match self.read_property(
                        host,
                        roots,
                        named,
                        PropertyKeyRef::JsString(&key),
                    )? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => return Ok(Err(error)),
                    };
                    if !matches!(value.view(), ValueRef::Undefined) {
                        match self.coerce_string(host, roots, &value)? {
                            Ok(value) => out.extend(value.units()),
                            Err(error) => return Ok(Err(error)),
                        }
                    }
                }
                _ => out.push(unit),
            }
        }
        Ok(Ok(JsString::from_units(out)))
    }

    fn regexp_flags<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let roots = &roots.with(&receiver);
        let flags =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("flags"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        self.coerce_string(host, roots, &flags)
    }

    fn regexp_last_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.read_property(
            host,
            roots,
            &Value::object(id),
            PropertyKeyRef::String("lastIndex"),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        match self.coerce_number(host, roots, &value)? {
            Ok(number) => Ok(Ok(to_integer(number))),
            Err(error) => Ok(Err(error)),
        }
    }

    fn regexp_set_last_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        value: f64,
    ) -> Result<Result<(), Value>, VmError> {
        match self.set_property(
            host,
            roots,
            id,
            &Value::object(id),
            PropertyKey::String("lastIndex".into()),
            Value::number(value),
            true,
        )? {
            Invocation::Value(_) => Ok(Ok(())),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    /// Reads `array[index]` for a match result or a named key such as `index`.
    fn regexp_element<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        array: ObjectId,
        key: &str,
    ) -> Result<Result<Value, Value>, VmError> {
        match self.read_property(
            host,
            roots,
            &Value::object(array),
            PropertyKeyRef::String(key),
        )? {
            Invocation::Value(value) => Ok(Ok(value)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn regexp_capture<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        array: ObjectId,
        index: usize,
    ) -> Result<Result<Value, Value>, VmError> {
        self.regexp_element(host, roots, array, &index.to_string())
    }

    fn regexp_string_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        key: &str,
    ) -> Result<Result<JsString, Value>, VmError> {
        let roots = &roots.with(&receiver);
        match self.read_property(host, roots, receiver, PropertyKeyRef::String(key))? {
            Invocation::Value(value) => self.coerce_string(host, roots, &value),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn regexp_test<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        input: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, input);
        let roots = &roots.with(&root_view);
        let input = match self.coerce_string(host, roots, input)? {
            Ok(input) => input,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match self.regexp_exec(host, roots, receiver, id, input)? {
            Invocation::Value(result) => Ok(Invocation::Value(Value::boolean(!matches!(
                result.view(),
                ValueRef::Null
            )))),
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }

    fn regexp_exec<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        input: JsString,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        self.regexp_exec_with_encoded(host, roots, receiver, id, input, None)
    }

    fn regexp_exec_with_encoded<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        input: JsString,
        encoded: Option<&[u16]>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        let exec =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("exec"))? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
        if self.is_callable(&exec) {
            return match self.invoke(
                host,
                roots,
                &exec,
                Arguments::Values(&[Value::string(input)]),
                receiver,
            )? {
                Invocation::Value(value)
                    if matches!(value.view(), ValueRef::Null | ValueRef::Object(_)) =>
                {
                    Ok(Invocation::Value(value))
                }
                thrown @ Invocation::Thrown(_) => Ok(thrown),
                Invocation::Value(_) => {
                    self.throw_type_error(roots, "RegExp exec must return an object or null")
                }
            };
        }
        if !matches!(self.heap.get(id)?, Object::RegExp { .. }) {
            return self.throw_type_error(
                roots,
                "RegExpExec requires a RegExp receiver or callable exec",
            );
        }
        self.regexp_match_with_encoded(host, roots, id, input, encoded)
    }

    fn regexp_match<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        input: JsString,
    ) -> Result<Invocation, VmError> {
        self.regexp_match_with_encoded(host, roots, id, input, None)
    }

    fn regexp_match_with_encoded<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        input: JsString,
        encoded: Option<&[u16]>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&id);
        let Object::RegExp { flags, .. } = self.heap.get(id)? else {
            unreachable!("RegExp receiver was checked before coercion");
        };
        let flags = *flags;
        let stateful = flags.global || flags.sticky;
        let receiver = Value::object(id);
        let last_index = match self.read_property(
            host,
            roots,
            &receiver,
            PropertyKeyRef::String("lastIndex"),
        )? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let last_index = match self.coerce_number(host, roots, &last_index)? {
            Ok(number) => number.clamp(0.0, 9_007_199_254_740_991.0) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let start = if stateful { last_index } else { 0 };
        let Object::RegExp { compiled, .. } = self.heap.get(id)? else {
            unreachable!("RegExp receiver was checked before coercion");
        };
        let encoded = encoded
            .map(Cow::Borrowed)
            .unwrap_or_else(|| encode_input(&input, flags));
        let matched = find_match(compiled, flags, &encoded, start);
        let groups = matched.as_ref().map(|matched| {
            let named = matched.named_groups().collect::<Vec<_>>();
            (0..=matched.captures.len())
                .map(|index| {
                    let range = matched.group(index);
                    let name = named.iter().find_map(|(name, candidate)| {
                        (*candidate == range && range.is_some()).then(|| (*name).to_owned())
                    });
                    (name, range)
                })
                .collect::<Vec<_>>()
        });
        if stateful {
            let next = matched.as_ref().map_or(0, |m| m.end());
            let write = self.set_property(
                host,
                roots,
                id,
                &receiver,
                "lastIndex".into(),
                Value::number(next as f64),
                true,
            )?;
            if let Invocation::Thrown(_) = write {
                return Ok(write);
            }
        }
        let Some(matched) = matched else {
            return Ok(Invocation::Value(Value::NULL));
        };
        let mut elements = Vec::new();
        let mut named = Vec::new();
        let mut index_elements = Vec::new();
        let mut named_indices = Vec::new();
        for (name, range) in groups.expect("exec match has capture ranges") {
            let value = range.as_ref().map_or(Value::UNDEFINED, |range| {
                Value::string(input.slice_units(range.clone()))
            });
            let pair = if flags.has_indices {
                range.map_or(Value::UNDEFINED, |range| {
                    Value::object(self.heap.alloc_array(vec![
                        Value::number(range.start as f64),
                        Value::number(range.end as f64),
                    ]))
                })
            } else {
                Value::UNDEFINED
            };
            if let Some(name) = name {
                named.push((name.clone(), value.clone()));
                if flags.has_indices {
                    named_indices.push((name, pair.clone()));
                }
            }
            elements.push(value);
            if flags.has_indices {
                index_elements.push(pair);
            }
        }
        let group_object = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let id = self.heap.alloc_plain_with(None);
            for (name, value) in named {
                self.heap.define_data(
                    id,
                    PropertyKey::String(name.into()),
                    value,
                    Attributes::ORDINARY,
                )?;
            }
            Value::object(id)
        };
        let indices = if flags.has_indices {
            let id = self.heap.alloc_array(index_elements);
            if !named_indices.is_empty() {
                let groups_id = self.heap.alloc_plain_with(None);
                for (name, value) in named_indices {
                    self.heap.define_data(
                        groups_id,
                        PropertyKey::String(name.into()),
                        value,
                        Attributes::ORDINARY,
                    )?;
                }
                self.heap.define_data(
                    id,
                    "groups",
                    Value::object(groups_id),
                    Attributes::ORDINARY,
                )?;
            }
            Some(Value::object(id))
        } else {
            None
        };
        let array = self.heap.alloc_array(elements);
        if let Some(indices) = indices {
            self.heap
                .define_data(array, "indices", indices, Attributes::ORDINARY)?;
        }
        for (key, value) in [
            ("index", Value::number(matched.start() as f64)),
            ("input", Value::string(input)),
            ("groups", group_object),
        ] {
            self.heap
                .define_data(array, key, value, Attributes::ORDINARY)?;
        }
        Ok(Invocation::Value(Value::object(array)))
    }
}
struct Substitution<'a> {
    string: &'a JsString,
    matched: &'a JsString,
    position: usize,
    captures: &'a [Value],
    named: &'a Value,
}

fn flags_contains(flags: &JsString, unit: u8) -> bool {
    flags.units().any(|flag| flag == u16::from(unit))
}

fn to_integer(number: f64) -> f64 {
    if number.is_nan() {
        0.0
    } else if number.is_finite() {
        number.trunc()
    } else {
        number
    }
}

fn to_length(number: f64) -> f64 {
    to_integer(number).clamp(0.0, 9_007_199_254_740_991.0) + 0.0
}

/// `AdvanceStringIndex`: one code unit, or one code point in Unicode modes.
fn advance_string_index(text: &JsString, index: f64, unicode: bool) -> f64 {
    let index = to_length(index);
    if unicode && index < text.unit_len() as f64 {
        let offset = index as usize;
        if matches!(text.unit_at(offset), Some(0xd800..=0xdbff))
            && matches!(text.unit_at(offset + 1), Some(0xdc00..=0xdfff))
        {
            return index + 2.0;
        }
    }
    index + 1.0
}

fn escaped_source(source: &JsString) -> JsString {
    if source.is_empty() {
        return "(?:)".into();
    }
    let mut escaped = Vec::with_capacity(source.unit_len());
    let mut in_class = false;
    let mut units = source.units().peekable();
    while let Some(unit) = units.next() {
        let escape = match unit {
            92 if units
                .peek()
                .is_some_and(|unit| !matches!(unit, 10 | 13 | 0x2028 | 0x2029)) =>
            {
                escaped.push(unit);
                escaped.push(units.next().expect("peeked a code unit"));
                continue;
            }
            92 if units.peek().is_some() => continue,
            47 if !in_class => "\\/",
            10 => "\\n",
            13 => "\\r",
            0x2028 => "\\u2028",
            0x2029 => "\\u2029",
            _ => {
                match unit {
                    91 => in_class = true,
                    93 => in_class = false,
                    _ => {}
                }
                escaped.push(unit);
                continue;
            }
        };
        escaped.extend(escape.encode_utf16());
    }
    JsString::from_units(escaped)
}

fn regexp_escape(input: &JsString) -> JsString {
    let mut units = input.units().enumerate().peekable();
    let mut out = Vec::new();
    while let Some((index, unit)) = units.next() {
        if index == 0 && unit <= 0x7f && (unit as u8).is_ascii_alphanumeric() {
            push_hex_escape(&mut out, 'x', unit);
        } else {
            match unit {
                0x0009 => out.extend("\\t".encode_utf16()),
                0x000a => out.extend("\\n".encode_utf16()),
                0x000b => out.extend("\\v".encode_utf16()),
                0x000c => out.extend("\\f".encode_utf16()),
                0x000d => out.extend("\\r".encode_utf16()),
                0x0020
                | 0x00a0
                | 0x1680
                | 0x2000..=0x200a
                | 0x2028..=0x2029
                | 0x202f
                | 0x205f
                | 0x3000
                | 0xfeff => push_hex_escape(&mut out, 'u', unit),
                0x0021 | 0x0022 | 0x0023 | 0x0024 | 0x0025 | 0x0026 | 0x0027 | 0x0028 | 0x0029
                | 0x002a | 0x002b | 0x002c | 0x002d | 0x002e | 0x002f | 0x003a | 0x003b
                | 0x003c | 0x003d | 0x003e | 0x003f | 0x0040 | 0x005b | 0x005c | 0x005d
                | 0x005e | 0x0060 | 0x007b | 0x007c | 0x007d | 0x007e => {
                    if matches!(
                        unit,
                        0x002c
                            | 0x002d
                            | 0x003d
                            | 0x0023
                            | 0x0026
                            | 0x0021
                            | 0x003a
                            | 0x003b
                            | 0x003c
                            | 0x003e
                            | 0x0040
                            | 0x007e
                            | 0x0027
                            | 0x0060
                            | 0x0022
                            | 0x0025
                    ) {
                        push_hex_escape(&mut out, 'x', unit);
                    } else {
                        out.extend([0x005c, unit]);
                    }
                }
                0xd800..=0xdbff
                    if units
                        .peek()
                        .is_some_and(|(_, next)| (0xdc00..=0xdfff).contains(next)) =>
                {
                    let (_, next) = units.next().expect("peeked a low surrogate");
                    out.extend([unit, next]);
                }
                0xd800..=0xdfff => push_hex_escape(&mut out, 'u', unit),
                _ => out.push(unit),
            }
        }
    }
    JsString::from_units(out)
}

fn push_hex_escape(out: &mut Vec<u16>, prefix: char, unit: u16) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.extend([0x005c, prefix as u16]);
    if prefix == 'u' {
        for shift in [12, 8, 4, 0] {
            out.push(u16::from(HEX[((unit >> shift) & 0xf) as usize]));
        }
    } else {
        for shift in [4, 0] {
            out.push(u16::from(HEX[((unit >> shift) & 0xf) as usize]));
        }
    }
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
