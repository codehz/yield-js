//! RegExp objects and matching over JavaScript UTF-16 code units.
use std::borrow::Cow;

use crate::builtin_globals::Owner;
use crate::builtins::{RootKind, RootSpec};
use crate::error::VmError;
use crate::heap::{Attributes, DescriptorUpdate, Object, PlainObject};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::regexp::{compile_pattern, encode_input, encode_input_with_flags, find_match};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

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

const REGEXP: IntrinsicKey = IntrinsicKey::new("RegExp", Slot::Global, "");

pub(crate) const OWNER: Owner = Owner::new(
    "regexp",
    REGEXP_ROOTS,
    &[REGEXP_INTRINSICS],
    Some(|vm, identity| {
        vm.install_regexp(
            vm.builtins.get("regexp"),
            vm.builtins.get("regexp.prototype"),
            vm.builtins.get("regexpIterator.prototype"),
            identity,
        );
    }),
);

pub(crate) const REGEXP_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "regexp",
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
        Some("object.prototype"),
    ),
];

macro_rules! regexp_method {
    ($slot:ident, $name:literal, $method:ident) => {
        IntrinsicDef::call(
            IntrinsicKey::new("RegExp", Slot::$slot, $name),
            |vm, host, args, receiver| {
                vm.call_regexp_method(host, RegExpMethod::$method, receiver, &args)
            },
        )
    };
}

pub(crate) const REGEXP_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        REGEXP,
        |vm, host, args, _| vm.construct_regexp(host, &args, None, true),
        |vm, host, args, prototype| vm.construct_regexp(host, &args, prototype, false),
    ),
    regexp_method!(Prototype, "exec", Exec),
    regexp_method!(Prototype, "test", Test),
    regexp_method!(Prototype, "toString", ToString),
    regexp_method!(PrototypeGetter, "source", Source),
    regexp_method!(PrototypeGetter, "flags", Flags),
    regexp_method!(PrototypeGetter, "global", Global),
    regexp_method!(PrototypeGetter, "ignoreCase", IgnoreCase),
    regexp_method!(PrototypeGetter, "multiline", Multiline),
    regexp_method!(PrototypeGetter, "dotAll", DotAll),
    regexp_method!(PrototypeGetter, "unicode", Unicode),
    regexp_method!(PrototypeGetter, "hasIndices", HasIndices),
    regexp_method!(PrototypeGetter, "unicodeSets", UnicodeSets),
    regexp_method!(PrototypeGetter, "sticky", Sticky),
    regexp_method!(Static, "escape", Escape),
    regexp_method!(PrototypeSymbol, "match", Match),
    regexp_method!(PrototypeSymbol, "matchAll", MatchAll),
    regexp_method!(PrototypeSymbol, "replace", Replace),
    regexp_method!(PrototypeSymbol, "search", Search),
    regexp_method!(PrototypeSymbol, "split", Split),
    IntrinsicDef::call(
        IntrinsicKey::new("RegExpStringIterator", Slot::Prototype, "next"),
        |vm, host, _, receiver| vm.regexp_string_iterator_next(host, receiver),
    ),
];

impl Vm {
    pub(crate) fn install_regexp(
        &mut self,
        constructor: ObjectId,
        prototype: ObjectId,
        iterator: ObjectId,
        identity: &Value,
    ) {
        self.heap
            .define_data(
                constructor,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("RegExp stores its prototype");
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(constructor),
                Attributes::METHOD,
            )
            .expect("RegExp.prototype stores its constructor");
        for def in REGEXP_INTRINSICS {
            let name = def.key.member;
            if def.key.slot == Slot::Global {
                continue;
            }
            let method = self.alloc_intrinsic(def.key);
            if def.key.slot == Slot::PrototypeGetter {
                assert!(
                    self.heap
                        .apply_descriptor(
                            prototype,
                            name.into(),
                            DescriptorUpdate {
                                get: Some(Some(method)),
                                configurable: Some(true),
                                ..DescriptorUpdate::default()
                            },
                        )
                        .expect("RegExp.prototype stores getters")
                );
                continue;
            }
            let (target, key) = match (def.key.owner, def.key.slot) {
                ("RegExp", Slot::Static) => (constructor, name.into()),
                ("RegExp", Slot::Prototype) => (prototype, name.into()),
                ("RegExpStringIterator", Slot::Prototype) => (iterator, name.into()),
                ("RegExp", Slot::PrototypeSymbol) => {
                    let symbol = match name {
                        "match" => crate::heap::MATCH_SYMBOL,
                        "matchAll" => crate::heap::MATCH_ALL_SYMBOL,
                        "replace" => crate::heap::REPLACE_SYMBOL,
                        "search" => crate::heap::SEARCH_SYMBOL,
                        "split" => crate::heap::SPLIT_SYMBOL,
                        _ => unreachable!("unexpected RegExp symbol: {name}"),
                    };
                    (prototype, PropertyKey::Symbol(symbol))
                }
                _ => unreachable!("unexpected RegExp declaration: {:?}", def.key),
            };
            self.heap
                .define_data(target, key, Value::Object(method), Attributes::METHOD)
                .expect("RegExp builtin method");
        }
        self.heap
            .define_data(
                iterator,
                PropertyKey::Symbol(crate::heap::ITERATOR_SYMBOL),
                identity.clone(),
                Attributes::METHOD,
            )
            .expect("RegExp iterator identity");
    }

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

    fn alloc_compiled_regexp(
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
            Value::Number(0.0),
            Attributes::FUNCTION_PROTOTYPE,
        )?;
        Ok(id)
    }

    pub(crate) fn construct_regexp(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
        prototype: Option<ObjectId>,
        called: bool,
    ) -> Result<Invocation, VmError> {
        let pattern = args.first().unwrap_or(&Value::Undefined);
        let flags_arg = args.get(1).unwrap_or(&Value::Undefined);
        let regexp = match pattern {
            Value::Object(id) => match self.heap.get(*id)? {
                Object::RegExp { .. } => Some(*id),
                _ => None,
            },
            _ => None,
        };
        if called && regexp.is_some() && matches!(flags_arg, Value::Undefined) {
            match self.read_property(host, pattern, PropertyKeyRef::String("constructor"))? {
                Invocation::Value(Value::Object(id)) if id == self.builtins.get("regexp") => {
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
                let source = if matches!(pattern, Value::Undefined) {
                    JsString::new()
                } else {
                    match self.coerce_string(host, pattern)? {
                        Ok(source) => source,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                (source, None)
            }
        };
        let flags_text = if matches!(flags_arg, Value::Undefined) {
            inherited_flags.map_or_else(JsString::new, |flags| flags.as_string().into())
        } else {
            match self.coerce_string(host, flags_arg)? {
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
                    .alloc_error(
                        self.builtins.get("syntaxerror.prototype"),
                        "SyntaxError",
                        Some(message.into()),
                    )
                    .map(Invocation::Thrown);
            }
        };
        let prototype = prototype.unwrap_or_else(|| self.builtins.get("regexp.prototype"));
        self.alloc_compiled_regexp(source, flags, compiled, prototype)
            .map(|id| Invocation::Value(Value::Object(id)))
    }

    pub(crate) fn call_regexp_method(
        &mut self,
        host: &mut dyn Host,
        method: RegExpMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        if matches!(method, RegExpMethod::Escape) {
            let Some(Value::String(input)) = args.first() else {
                return self.throw_type_error("RegExp.escape requires a string argument");
            };
            return Ok(Invocation::Value(Value::String(regexp_escape(input))));
        }
        let Value::Object(id) = *receiver else {
            return self.throw_type_error("RegExp method requires an object receiver");
        };
        match method {
            RegExpMethod::ToString => {
                let source = match self.regexp_string_property(host, receiver, "source")? {
                    Ok(source) => source,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let flags = match self.regexp_string_property(host, receiver, "flags")? {
                    Ok(flags) => flags,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let mut text = JsString::from("/");
                text.push_str(&source);
                text.push('/');
                text.push_str(&flags);
                return Ok(Invocation::Value(Value::String(text)));
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
                    match self.read_property(host, receiver, PropertyKeyRef::String(key))? {
                        Invocation::Value(value) => {
                            if value.truthy() {
                                flags.push(flag);
                            }
                        }
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    }
                }
                return Ok(Invocation::Value(Value::String(flags.into())));
            }
            RegExpMethod::Test => {
                return self.regexp_test(
                    host,
                    receiver,
                    id,
                    args.first().unwrap_or(&Value::Undefined),
                );
            }
            RegExpMethod::Match => return self.regexp_symbol_match(host, id, args),
            RegExpMethod::MatchAll => return self.regexp_symbol_match_all(host, id, args),
            RegExpMethod::Replace => return self.regexp_symbol_replace(host, id, args),
            RegExpMethod::Search => return self.regexp_symbol_search(host, id, args),
            RegExpMethod::Split => return self.regexp_symbol_split(host, id, args),
            _ => {}
        }
        let Object::RegExp { source, flags, .. } = self.heap.get(id)? else {
            if id == self.builtins.get("regexp.prototype") {
                match method {
                    RegExpMethod::Source => {
                        return Ok(Invocation::Value(Value::String("(?:)".into())));
                    }
                    RegExpMethod::Exec => {}
                    _ => return Ok(Invocation::Value(Value::Undefined)),
                }
            }
            return self.throw_type_error("RegExp method requires a RegExp receiver");
        };
        let value = match method {
            RegExpMethod::Exec => {
                let argument = args.first().unwrap_or(&Value::Undefined);
                let input = match self.coerce_string(host, argument)? {
                    Ok(input) => input,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                return self.regexp_match(host, id, input);
            }
            RegExpMethod::Source => Value::String(escaped_source(source)),
            RegExpMethod::Global => Value::Bool(flags.global),
            RegExpMethod::IgnoreCase => Value::Bool(flags.ignore_case),
            RegExpMethod::Multiline => Value::Bool(flags.multiline),
            RegExpMethod::DotAll => Value::Bool(flags.dot_all),
            RegExpMethod::HasIndices => Value::Bool(flags.has_indices),
            RegExpMethod::Sticky => Value::Bool(flags.sticky),
            RegExpMethod::Unicode => Value::Bool(flags.unicode),
            RegExpMethod::UnicodeSets => Value::Bool(flags.unicode_sets),
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
    fn regexp_symbol_match(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver = Value::Object(id);
        let string = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let flags = match self.regexp_flags(host, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if !flags_contains(&flags, b'g') {
            return self.regexp_exec(host, &receiver, id, string);
        }
        if let Err(error) = self.regexp_set_last_index(host, id, 0.0)? {
            return Ok(Invocation::Thrown(error));
        }
        let unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let encoded = encode_input_with_flags(&string, &flags).into_owned();
        let mut results = Vec::new();
        loop {
            match self.regexp_exec_with_encoded(
                host,
                &receiver,
                id,
                string.clone(),
                Some(encoded.as_ref()),
            )? {
                Invocation::Value(Value::Null) => break,
                Invocation::Value(Value::Object(array)) => {
                    let matched = match self.regexp_capture(host, array, 0)? {
                        Ok(value) => match self.coerce_string(host, &value)? {
                            Ok(matched) => matched,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        },
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                    results.push(Value::String(matched.clone()));
                    if matched.is_empty() {
                        let last_index = match self.regexp_last_index(host, id)? {
                            Ok(index) => index,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        let next = advance_string_index(&string, last_index, unicode) as f64;
                        if let Err(error) = self.regexp_set_last_index(host, id, next)? {
                            return Ok(Invocation::Thrown(error));
                        }
                    }
                }
                Invocation::Value(_) => {
                    return self.throw_type_error("RegExp exec must return an object or null");
                }
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        if results.is_empty() {
            return Ok(Invocation::Value(Value::Null));
        }
        self.array_result(results)
    }

    fn regexp_symbol_match_all(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver = Value::Object(id);
        let string = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let flags = match self.regexp_flags(host, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if !flags_contains(&flags, b'g') {
            return self.throw_type_error("String.prototype.matchAll requires a global RegExp");
        }
        let created = self.construct_regexp(
            host,
            &[receiver.clone(), Value::String(flags.clone())],
            None,
            false,
        )?;
        let Invocation::Value(Value::Object(matcher)) = created else {
            return Ok(created);
        };
        let last_index =
            match self.read_property(host, &receiver, PropertyKeyRef::String("lastIndex"))? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
        if let thrown @ Invocation::Thrown(_) = self.set_property(
            host,
            matcher,
            &Value::Object(matcher),
            PropertyKey::String("lastIndex".into()),
            last_index,
            true,
        )? {
            return Ok(thrown);
        }
        let full_unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let iterator = self.heap.alloc(Object::RegExpStringIterator {
            object: PlainObject::new(Some(self.builtins.get("regexpIterator.prototype"))),
            regexp: matcher,
            string,
            global: true,
            full_unicode,
            done: false,
        });
        Ok(Invocation::Value(Value::Object(iterator)))
    }

    fn regexp_string_iterator_next(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid = "RegExpStringIterator.next requires a RegExp string iterator";
        let Value::Object(id) = *receiver else {
            return self.throw_type_error(invalid);
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
            return self.throw_type_error(invalid);
        };
        let (regexp, string, global, full_unicode, done) =
            (*regexp, string.clone(), *global, *full_unicode, *done);
        if done {
            return self.iterator_result(Value::Undefined, true);
        }
        let matcher = Value::Object(regexp);
        match self.regexp_exec(host, &matcher, regexp, string.clone())? {
            Invocation::Value(Value::Null) => {
                let Object::RegExpStringIterator { done, .. } = self.heap.get_mut(id)? else {
                    unreachable!("iterator kind does not change");
                };
                *done = true;
                self.iterator_result(Value::Undefined, true)
            }
            Invocation::Value(result @ Value::Object(array)) => {
                if global {
                    let matched = match self.regexp_capture(host, array, 0)? {
                        Ok(value) => match self.coerce_string(host, &value)? {
                            Ok(matched) => matched,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        },
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                    if matched.is_empty() {
                        let last_index = match self.regexp_last_index(host, regexp)? {
                            Ok(index) => index,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        let next = advance_string_index(&string, last_index, full_unicode) as f64;
                        if let Err(error) = self.regexp_set_last_index(host, regexp, next)? {
                            return Ok(Invocation::Thrown(error));
                        }
                    }
                }
                self.iterator_result(result, false)
            }
            Invocation::Value(_) => {
                self.throw_type_error("RegExp exec must return an object or null")
            }
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }

    fn regexp_symbol_replace(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver = Value::Object(id);
        let string = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let replace_value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let functional = self.is_callable(&replace_value);
        let replacement = if functional {
            JsString::default()
        } else {
            match self.coerce_string(host, &replace_value)? {
                Ok(replacement) => replacement,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let flags = match self.regexp_flags(host, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let global = flags_contains(&flags, b'g');
        let unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let mut results = Vec::new();
        if global {
            if let Err(error) = self.regexp_set_last_index(host, id, 0.0)? {
                return Ok(Invocation::Thrown(error));
            }
            let encoded = encode_input_with_flags(&string, &flags).into_owned();
            loop {
                match self.regexp_exec_with_encoded(
                    host,
                    &receiver,
                    id,
                    string.clone(),
                    Some(encoded.as_ref()),
                )? {
                    Invocation::Value(Value::Null) => break,
                    Invocation::Value(Value::Object(array)) => {
                        results.push(Value::Object(array));
                        let matched = match self.regexp_capture(host, array, 0)? {
                            Ok(value) => match self.coerce_string(host, &value)? {
                                Ok(matched) => matched,
                                Err(error) => return Ok(Invocation::Thrown(error)),
                            },
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        };
                        if matched.is_empty() {
                            let last_index = match self.regexp_last_index(host, id)? {
                                Ok(index) => index,
                                Err(error) => return Ok(Invocation::Thrown(error)),
                            };
                            let next = advance_string_index(&string, last_index, unicode) as f64;
                            if let Err(error) = self.regexp_set_last_index(host, id, next)? {
                                return Ok(Invocation::Thrown(error));
                            }
                        }
                    }
                    Invocation::Value(_) => {
                        return self.throw_type_error("RegExp exec must return an object or null");
                    }
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            }
        } else {
            match self.regexp_exec(host, &receiver, id, string.clone())? {
                Invocation::Value(Value::Null) => {
                    return Ok(Invocation::Value(Value::String(string)));
                }
                Invocation::Value(Value::Object(array)) => results.push(Value::Object(array)),
                Invocation::Value(_) => {
                    return self.throw_type_error("RegExp exec must return an object or null");
                }
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        if results.is_empty() {
            return Ok(Invocation::Value(Value::String(string)));
        }
        match self.regexp_replace_results(
            host,
            &string,
            results,
            functional,
            &replace_value,
            &replacement,
        )? {
            Ok(out) => Ok(Invocation::Value(Value::String(out))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn regexp_symbol_search(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver = Value::Object(id);
        let string = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let previous = match self.regexp_last_index(host, id)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if previous.to_bits() != 0.0f64.to_bits()
            && let Err(error) = self.regexp_set_last_index(host, id, 0.0)?
        {
            return Ok(Invocation::Thrown(error));
        }
        let result = self.regexp_exec(host, &receiver, id, string)?;
        let current = match self.regexp_last_index(host, id)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if current.to_bits() != previous.to_bits()
            && let Err(error) = self.regexp_set_last_index(host, id, previous)?
        {
            return Ok(Invocation::Thrown(error));
        }
        match result {
            Invocation::Value(Value::Null) => Ok(Invocation::Value(Value::Number(-1.0))),
            Invocation::Value(Value::Object(array)) => {
                match self.regexp_element(host, array, "index")? {
                    Ok(value) => match self.coerce_number(host, &value)? {
                        Ok(number) => Ok(Invocation::Value(Value::Number(to_integer(number)))),
                        Err(error) => Ok(Invocation::Thrown(error)),
                    },
                    Err(error) => Ok(Invocation::Thrown(error)),
                }
            }
            Invocation::Value(_) => {
                self.throw_type_error("RegExp exec must return an object or null")
            }
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }

    fn regexp_symbol_split(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver = Value::Object(id);
        let string = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let limit = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.coerce_number(host, value)? {
                    Ok(number) => crate::ops::number_to_uint32(number) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => u32::MAX as usize,
        };
        let flags = match self.regexp_flags(host, &receiver)? {
            Ok(flags) => flags,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let unicode = flags_contains(&flags, b'u') || flags_contains(&flags, b'v');
        let mut splitter_flags = flags.clone();
        if !flags_contains(&flags, b'y') {
            splitter_flags.push('y');
        }
        let created = self.construct_regexp(
            host,
            &[receiver.clone(), Value::String(splitter_flags)],
            None,
            false,
        )?;
        let Invocation::Value(Value::Object(splitter)) = created else {
            return Ok(created);
        };
        let size = string.unit_len();
        let encoded = encode_input_with_flags(&string, &flags).into_owned();
        let mut parts = Vec::new();
        if limit == 0 {
            return self.array_result(parts);
        }
        if size == 0 {
            match self.regexp_exec_with_encoded(
                host,
                &Value::Object(splitter),
                splitter,
                string.clone(),
                Some(encoded.as_ref()),
            )? {
                Invocation::Value(Value::Null) => parts.push(Value::String(string)),
                Invocation::Value(_) => {}
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
            return self.array_result(parts);
        }
        let mut last = 0usize;
        let mut start = 0usize;
        let mut cursor = 0usize;
        while cursor < size {
            if let Err(error) = self.regexp_set_last_index(host, splitter, cursor as f64)? {
                return Ok(Invocation::Thrown(error));
            }
            let result = self.regexp_exec_with_encoded(
                host,
                &Value::Object(splitter),
                splitter,
                string.clone(),
                Some(encoded.as_ref()),
            )?;
            let Invocation::Value(result) = result else {
                return Ok(result);
            };
            let Value::Object(array) = result else {
                cursor = advance_string_index(&string, cursor as f64, unicode);
                continue;
            };
            let index = match self.regexp_element(host, array, "index")? {
                Ok(value) => match self.coerce_number(host, &value)? {
                    Ok(number) => to_integer(number).max(0.0) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let end = match self.regexp_capture(host, array, 0)? {
                Ok(value) => match self.coerce_string(host, &value)? {
                    Ok(matched) => (index + matched.unit_len()).min(size),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if end == start {
                cursor = advance_string_index(&string, cursor as f64, unicode);
                continue;
            }
            let index = index.min(size);
            if start <= index {
                parts.push(Value::String(string.slice_units(last.min(index)..index)));
            } else {
                parts.push(Value::String(JsString::default()));
            }
            if parts.len() == limit {
                return self.array_result(parts);
            }
            last = end;
            start = end;
            cursor = end;
            let count = match self.regexp_element(host, array, "length")? {
                Ok(value) => match self.coerce_number(host, &value)? {
                    Ok(number) => to_integer(number).clamp(0.0, u32::MAX as f64) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            for index in 1..count {
                let value = match self.regexp_capture(host, array, index)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                parts.push(value);
                if parts.len() == limit {
                    return self.array_result(parts);
                }
            }
        }
        if last <= size {
            parts.push(Value::String(string.slice_units(last..size)));
        }
        self.array_result(parts)
    }

    fn regexp_replace_results(
        &mut self,
        host: &mut dyn Host,
        string: &JsString,
        results: Vec<Value>,
        functional: bool,
        replace_value: &Value,
        replacement: &JsString,
    ) -> Result<Result<JsString, Value>, VmError> {
        let length = string.unit_len();
        let mut out = Vec::new();
        let mut next = 0usize;
        for result in results {
            let Value::Object(array) = result else {
                continue;
            };
            let index = match self.regexp_element(host, array, "index")? {
                Ok(value) => match self.coerce_number(host, &value)? {
                    Ok(number) => to_integer(number),
                    Err(error) => return Ok(Err(error)),
                },
                Err(error) => return Ok(Err(error)),
            };
            let matched = match self.regexp_capture(host, array, 0)? {
                Ok(value) => match self.coerce_string(host, &value)? {
                    Ok(matched) => matched,
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
            let count = match self.regexp_element(host, array, "length")? {
                Ok(value) => match self.coerce_number(host, &value)? {
                    Ok(number) => to_integer(number).clamp(0.0, u32::MAX as f64) as usize,
                    Err(error) => return Ok(Err(error)),
                },
                Err(error) => return Ok(Err(error)),
            };
            let mut captures = Vec::new();
            for index in 1..count {
                match self.regexp_capture(host, array, index)? {
                    Ok(value) => captures.push(value),
                    Err(error) => return Ok(Err(error)),
                }
            }
            let groups = match self.regexp_element(host, array, "groups")? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            let start = next.min(position);
            out.extend(string.units().skip(start).take(position - start));
            if functional {
                let mut argv = Vec::with_capacity(captures.len() + 4);
                argv.push(Value::String(matched.clone()));
                argv.extend(captures.iter().cloned());
                argv.push(Value::Number(position as f64));
                argv.push(Value::String(string.clone()));
                if !matches!(groups, Value::Undefined) {
                    argv.push(groups.clone());
                }
                let value = match self.invoke(host, replace_value, argv, &Value::Undefined)? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => match thrown {
                        Invocation::Thrown(error) => return Ok(Err(error)),
                        _ => unreachable!(),
                    },
                };
                match self.coerce_string(host, &value)? {
                    Ok(value) => out.extend(value.units()),
                    Err(error) => return Ok(Err(error)),
                }
            } else {
                let parts = Substitution {
                    string,
                    matched: &matched,
                    position,
                    captures: &captures,
                    named: &groups,
                };
                match self.regexp_substitution(host, replacement, &parts)? {
                    Ok(value) => out.extend(value.units()),
                    Err(error) => return Ok(Err(error)),
                }
            }
            next = tail;
        }
        out.extend(string.units().skip(next.min(length)));
        Ok(Ok(JsString::from_units(out)))
    }

    fn regexp_substitution(
        &mut self,
        host: &mut dyn Host,
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
                        if !matches!(captures[number - 1], Value::Undefined) {
                            match self.coerce_string(host, &captures[number - 1])? {
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
                    if matches!(named, Value::Undefined) {
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
                    let value =
                        match self.read_property(host, named, PropertyKeyRef::JsString(&key))? {
                            Invocation::Value(value) => value,
                            Invocation::Thrown(error) => return Ok(Err(error)),
                        };
                    if !matches!(value, Value::Undefined) {
                        match self.coerce_string(host, &value)? {
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

    fn regexp_flags(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let flags = match self.read_property(host, receiver, PropertyKeyRef::String("flags"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        self.coerce_string(host, &flags)
    }

    fn regexp_last_index(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.read_property(
            host,
            &Value::Object(id),
            PropertyKeyRef::String("lastIndex"),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        match self.coerce_number(host, &value)? {
            Ok(number) => Ok(Ok(to_integer(number))),
            Err(error) => Ok(Err(error)),
        }
    }

    fn regexp_set_last_index(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        value: f64,
    ) -> Result<Result<(), Value>, VmError> {
        match self.set_property(
            host,
            id,
            &Value::Object(id),
            PropertyKey::String("lastIndex".into()),
            Value::Number(value),
            true,
        )? {
            Invocation::Value(_) => Ok(Ok(())),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    /// Reads `array[index]` for a match result or a named key such as `index`.
    fn regexp_element(
        &mut self,
        host: &mut dyn Host,
        array: ObjectId,
        key: &str,
    ) -> Result<Result<Value, Value>, VmError> {
        match self.read_property(host, &Value::Object(array), PropertyKeyRef::String(key))? {
            Invocation::Value(value) => Ok(Ok(value)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn regexp_capture(
        &mut self,
        host: &mut dyn Host,
        array: ObjectId,
        index: usize,
    ) -> Result<Result<Value, Value>, VmError> {
        self.regexp_element(host, array, &index.to_string())
    }

    fn regexp_string_property(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        key: &str,
    ) -> Result<Result<JsString, Value>, VmError> {
        match self.read_property(host, receiver, PropertyKeyRef::String(key))? {
            Invocation::Value(value) => self.coerce_string(host, &value),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn regexp_test(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        id: ObjectId,
        input: &Value,
    ) -> Result<Invocation, VmError> {
        let input = match self.coerce_string(host, input)? {
            Ok(input) => input,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match self.regexp_exec(host, receiver, id, input)? {
            Invocation::Value(result) => Ok(Invocation::Value(Value::Bool(result != Value::Null))),
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }

    fn regexp_exec(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        id: ObjectId,
        input: JsString,
    ) -> Result<Invocation, VmError> {
        self.regexp_exec_with_encoded(host, receiver, id, input, None)
    }

    fn regexp_exec_with_encoded(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        id: ObjectId,
        input: JsString,
        encoded: Option<&[u16]>,
    ) -> Result<Invocation, VmError> {
        let exec = match self.read_property(host, receiver, PropertyKeyRef::String("exec"))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        if self.is_callable(&exec) {
            return match self.invoke(host, &exec, vec![Value::String(input)], receiver)? {
                result @ (Invocation::Value(Value::Null | Value::Object(_))
                | Invocation::Thrown(_)) => Ok(result),
                Invocation::Value(_) => {
                    self.throw_type_error("RegExp exec must return an object or null")
                }
            };
        }
        if !matches!(self.heap.get(id)?, Object::RegExp { .. }) {
            return self.throw_type_error("RegExpExec requires a RegExp receiver or callable exec");
        }
        self.regexp_match_with_encoded(host, id, input, encoded)
    }

    fn regexp_match(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        input: JsString,
    ) -> Result<Invocation, VmError> {
        self.regexp_match_with_encoded(host, id, input, None)
    }

    fn regexp_match_with_encoded(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        input: JsString,
        encoded: Option<&[u16]>,
    ) -> Result<Invocation, VmError> {
        let Object::RegExp { flags, .. } = self.heap.get(id)? else {
            unreachable!("RegExp receiver was checked before coercion");
        };
        let flags = *flags;
        let stateful = flags.global || flags.sticky;
        let receiver = Value::Object(id);
        let last_index =
            match self.read_property(host, &receiver, PropertyKeyRef::String("lastIndex"))? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
        let last_index = match self.coerce_number(host, &last_index)? {
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
                id,
                &receiver,
                "lastIndex".into(),
                Value::Number(next as f64),
                true,
            )?;
            if let Invocation::Thrown(_) = write {
                return Ok(write);
            }
        }
        let Some(matched) = matched else {
            return Ok(Invocation::Value(Value::Null));
        };
        let mut elements = Vec::new();
        let mut named = Vec::new();
        let mut index_elements = Vec::new();
        let mut named_indices = Vec::new();
        for (name, range) in groups.expect("exec match has capture ranges") {
            let value = range.as_ref().map_or(Value::Undefined, |range| {
                Value::String(input.slice_units(range.clone()))
            });
            let pair = if flags.has_indices {
                range.map_or(Value::Undefined, |range| {
                    Value::Object(self.heap.alloc_array(vec![
                        Value::Number(range.start as f64),
                        Value::Number(range.end as f64),
                    ]))
                })
            } else {
                Value::Undefined
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
            Value::Undefined
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
            Value::Object(id)
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
                    Value::Object(groups_id),
                    Attributes::ORDINARY,
                )?;
            }
            Some(Value::Object(id))
        } else {
            None
        };
        let array = self.heap.alloc_array(elements);
        if let Some(indices) = indices {
            self.heap
                .define_data(array, "indices", indices, Attributes::ORDINARY)?;
        }
        for (key, value) in [
            ("index", Value::Number(matched.start() as f64)),
            ("input", Value::String(input)),
            ("groups", group_object),
        ] {
            self.heap
                .define_data(array, key, value, Attributes::ORDINARY)?;
        }
        Ok(Invocation::Value(Value::Object(array)))
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

/// `AdvanceStringIndex`: one code unit, or one code point in Unicode modes.
fn advance_string_index(text: &JsString, index: f64, unicode: bool) -> usize {
    let length = text.unit_len();
    let index = if index.is_nan() || index < 0.0 {
        0
    } else if index > length as f64 {
        length
    } else {
        index as usize
    };
    if !unicode {
        return index + 1;
    }
    match text.unit_at(index) {
        Some(first) if (0xd800..=0xdbff).contains(&first) => match text.unit_at(index + 1) {
            Some(second) if (0xdc00..=0xdfff).contains(&second) => index + 2,
            _ => index + 1,
        },
        _ => index + 1,
    }
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
