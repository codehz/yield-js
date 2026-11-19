//! RegExp objects and matching over JavaScript UTF-16 code units.

use crate::error::VmError;
use crate::heap::{Attributes, DescriptorUpdate, Object, PlainObject};
use crate::host::Host;
use crate::regexp::{compile_pattern, encode_input};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
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
    Sticky,
}

pub(crate) const REGEXP_METHODS: &[(&str, RegExpMethod)] = &[
    ("exec", RegExpMethod::Exec),
    ("test", RegExpMethod::Test),
    ("toString", RegExpMethod::ToString),
    ("get source", RegExpMethod::Source),
    ("get flags", RegExpMethod::Flags),
    ("get global", RegExpMethod::Global),
    ("get ignoreCase", RegExpMethod::IgnoreCase),
    ("get multiline", RegExpMethod::Multiline),
    ("get dotAll", RegExpMethod::DotAll),
    ("get unicode", RegExpMethod::Unicode),
    ("get sticky", RegExpMethod::Sticky),
];

impl Vm {
    pub(crate) fn init_regexp(&mut self) -> (ObjectId, ObjectId) {
        let constructor = self.heap.alloc_builtin("RegExp", None);
        let prototype = self.heap.alloc_plain();
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
        for &(name, _) in REGEXP_METHODS {
            let method = self.heap.alloc_builtin("RegExp", Some(name));
            if let Some(property) = name.strip_prefix("get ") {
                assert!(
                    self.heap
                        .apply_descriptor(
                            prototype,
                            property.into(),
                            DescriptorUpdate {
                                get: Some(Some(method)),
                                configurable: Some(true),
                                ..DescriptorUpdate::default()
                            },
                        )
                        .expect("RegExp.prototype stores getters")
                );
            } else {
                self.heap
                    .define_data(prototype, name, Value::Object(method), Attributes::METHOD)
                    .expect("RegExp.prototype stores methods");
            }
        }
        (constructor, prototype)
    }

    /// A literal was validated by the compiler, but bytecode may come from elsewhere.
    pub(crate) fn alloc_regexp(&mut self, source: &str, flags: &str) -> Result<ObjectId, VmError> {
        let (flags, compiled) = compile_pattern(source, flags).map_err(VmError::InvalidBuild)?;
        self.alloc_compiled_regexp(
            source.to_owned(),
            flags,
            compiled,
            self.builtins.regexp_prototype,
        )
    }

    fn alloc_compiled_regexp(
        &mut self,
        source: String,
        flags: crate::regexp::RegExpFlags,
        compiled: regex::Regex,
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
                Invocation::Value(Value::Object(id)) if id == self.builtins.regexp => {
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
                    String::new()
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
            inherited_flags.map_or_else(String::new, |flags| flags.as_string())
        } else {
            match self.coerce_string(host, flags_arg)? {
                Ok(flags) => flags,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let (flags, compiled) = match compile_pattern(&source, &flags_text) {
            Ok(compiled) => compiled,
            Err(message) => {
                return self
                    .alloc_error(
                        self.builtins.syntaxerror_prototype,
                        "SyntaxError",
                        Some(message),
                    )
                    .map(Invocation::Thrown);
            }
        };
        let prototype = prototype.unwrap_or(self.builtins.regexp_prototype);
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
                return Ok(Invocation::Value(Value::String(format!(
                    "/{source}/{flags}"
                ))));
            }
            RegExpMethod::Flags => {
                let mut flags = String::new();
                for (key, flag) in [
                    ("global", 'g'),
                    ("ignoreCase", 'i'),
                    ("multiline", 'm'),
                    ("dotAll", 's'),
                    ("unicode", 'u'),
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
                return Ok(Invocation::Value(Value::String(flags)));
            }
            RegExpMethod::Test => {
                return self.regexp_test(
                    host,
                    receiver,
                    id,
                    args.first().unwrap_or(&Value::Undefined),
                );
            }
            _ => {}
        }
        let Object::RegExp { source, flags, .. } = self.heap.get(id)? else {
            if id == self.builtins.regexp_prototype {
                match method {
                    RegExpMethod::Source => {
                        return Ok(Invocation::Value(Value::String("(?:)".to_owned())));
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
                return self.regexp_match(host, id, input, false);
            }
            RegExpMethod::Source => Value::String(escaped_source(source)),
            RegExpMethod::Global => Value::Bool(flags.global),
            RegExpMethod::IgnoreCase => Value::Bool(flags.ignore_case),
            RegExpMethod::Multiline => Value::Bool(flags.multiline),
            RegExpMethod::DotAll => Value::Bool(flags.dot_all),
            RegExpMethod::Unicode => Value::Bool(false),
            RegExpMethod::Sticky => Value::Bool(flags.sticky),
            RegExpMethod::Test | RegExpMethod::ToString | RegExpMethod::Flags => unreachable!(),
        };
        Ok(Invocation::Value(value))
    }

    fn regexp_string_property(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        key: &str,
    ) -> Result<Result<String, Value>, VmError> {
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
        let exec = match self.read_property(host, receiver, PropertyKeyRef::String("exec"))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        if self.is_callable(&exec) {
            let Value::Object(exec_id) = exec else {
                unreachable!("callable is an object");
            };
            // The built-in needs no materialized captures when called by test.
            if matches!(self.heap.get(exec_id)?, Object::BuiltinStatic(function)
                if function.name == "RegExp" && function.member.as_deref() == Some("exec"))
                && matches!(self.heap.get(id)?, Object::RegExp { .. })
            {
                return self.regexp_match(host, id, input, true);
            }
            return match self.invoke(host, &exec, vec![Value::String(input)], receiver)? {
                Invocation::Value(Value::Null) => Ok(Invocation::Value(Value::Bool(false))),
                Invocation::Value(Value::Object(_)) => Ok(Invocation::Value(Value::Bool(true))),
                Invocation::Value(_) => {
                    self.throw_type_error("RegExp exec must return an object or null")
                }
                thrown @ Invocation::Thrown(_) => Ok(thrown),
            };
        }
        if !matches!(self.heap.get(id)?, Object::RegExp { .. }) {
            return self
                .throw_type_error("RegExp test requires a RegExp receiver or callable exec");
        }
        self.regexp_match(host, id, input, true)
    }

    fn regexp_match(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        input: String,
        test: bool,
    ) -> Result<Invocation, VmError> {
        let Object::RegExp { flags, .. } = self.heap.get(id)? else {
            unreachable!("RegExp receiver was checked before coercion");
        };
        let flags = *flags;
        let stateful = flags.global || flags.sticky;
        let receiver = Value::Object(id);
        let start = if stateful {
            let key = PropertyKeyRef::String("lastIndex");
            let last_index = match self.read_property(host, &receiver, key)? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            match self.coerce_number(host, &last_index)? {
                // A saturating cast is sufficient: indices past the input always fail.
                Ok(number) => number as usize,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        } else {
            0
        };
        let encoded = encode_input(&input, flags);
        let Object::RegExp { compiled, .. } = self.heap.get(id)? else {
            unreachable!("RegExp receiver was checked before coercion");
        };
        if test && !stateful {
            return Ok(Invocation::Value(Value::Bool(compiled.is_match(&encoded))));
        }
        // The encoder emits exactly one scalar per original UTF-16 code unit.
        let offsets = (!encoded.is_ascii()).then(|| {
            encoded
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(encoded.len()))
                .collect::<Vec<_>>()
        });
        let start_byte = match &offsets {
            Some(offsets) => offsets.get(start).copied(),
            None => (start <= encoded.len()).then_some(start),
        };
        let unit_offset = |byte| match &offsets {
            Some(offsets) => offsets
                .binary_search(&byte)
                .expect("match is at a scalar boundary"),
            None => byte,
        };
        let captures = if test {
            None
        } else {
            start_byte.and_then(|byte| compiled.captures_at(&encoded, byte))
        };
        let matched = if test {
            start_byte.and_then(|byte| compiled.find_at(&encoded, byte))
        } else {
            captures.as_ref().and_then(|captures| captures.get(0))
        }
        .filter(|matched| !flags.sticky || start_byte == Some(matched.start()));

        // Collect the ranges before releasing the immutable engine borrow for lastIndex.
        let groups = matched.and(captures.as_ref()).map(|captures| {
            compiled
                .capture_names()
                .enumerate()
                .map(|(i, name)| {
                    let range = captures
                        .get(i)
                        .map(|m| unit_offset(m.start())..unit_offset(m.end()));
                    (name.map(str::to_owned), range)
                })
                .collect::<Vec<_>>()
        });
        if stateful {
            let next = matched.map_or(0, |m| {
                unit_offset(m.end()) + usize::from(m.start() == m.end())
            });
            let write = self.set_property(
                host,
                id,
                &receiver,
                PropertyKey::String("lastIndex".to_owned()),
                Value::Number(next as f64),
                true,
            )?;
            if let Invocation::Thrown(_) = write {
                return Ok(write);
            }
        }
        if test {
            return Ok(Invocation::Value(Value::Bool(matched.is_some())));
        }
        let Some(matched) = matched else {
            return Ok(Invocation::Value(Value::Null));
        };
        let units = (!input.is_ascii()).then(|| input.encode_utf16().collect::<Vec<_>>());
        let mut elements = Vec::new();
        let mut named = Vec::new();
        for (name, range) in groups.expect("exec match has capture ranges") {
            let value = match range {
                Some(range) => {
                    let text = match &units {
                        Some(units) => match String::from_utf16(&units[range]) {
                            Ok(string) => string,
                            Err(_) => {
                                return self.throw_type_error(
                                    "RegExp capture cannot represent a lone surrogate",
                                );
                            }
                        },
                        None => input[range].to_owned(),
                    };
                    Value::String(text)
                }
                None => Value::Undefined,
            };
            if let Some(name) = name {
                named.push((name, value.clone()));
            }
            elements.push(value);
        }
        let group_object = if named.is_empty() {
            Value::Undefined
        } else {
            let id = self.heap.alloc_plain_with(None);
            for (name, value) in named {
                self.heap.define_data(
                    id,
                    PropertyKey::String(name),
                    value,
                    Attributes::ORDINARY,
                )?;
            }
            Value::Object(id)
        };
        let array = self.heap.alloc_array(elements);
        for (key, value) in [
            ("index", Value::Number(unit_offset(matched.start()) as f64)),
            ("input", Value::String(input)),
            ("groups", group_object),
        ] {
            self.heap
                .define_data(array, key, value, Attributes::ORDINARY)?;
        }
        Ok(Invocation::Value(Value::Object(array)))
    }
}

fn escaped_source(source: &str) -> String {
    if source.is_empty() {
        return "(?:)".to_owned();
    }
    let mut escaped = String::with_capacity(source.len());
    let mut in_class = false;
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek().is_some_and(|c| !is_line_terminator(*c)) => {
                escaped.push(ch);
                escaped.push(chars.next().expect("peeked a character"));
            }
            '\\' if chars.peek().is_some() => {}
            '/' if !in_class => escaped.push_str("\\/"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => {
                match ch {
                    '[' => in_class = true,
                    ']' => in_class = false,
                    _ => {}
                }
                escaped.push(ch);
            }
        }
    }
    escaped
}

fn is_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}
