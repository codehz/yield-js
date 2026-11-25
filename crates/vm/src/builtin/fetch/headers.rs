//! Fetch header lists preserve byte values and insertion order independently of the JS view.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data,
    globals::Owner,
    iterator::SYNC_ROOT,
    link_constructor,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ITERATOR_SYMBOL, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

pub(crate) type HeaderPair = (Vec<u8>, Vec<u8>);
type Completion<T> = Result<Result<T, Value>, VmError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum HeadersGuard {
    None,
    Immutable,
    Request,
    RequestNoCors,
    Response,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeadersRecord {
    pub(crate) list: Vec<HeaderPair>,
    pub(crate) guard: HeadersGuard,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum HeadersIteratorKind {
    Entries,
    Keys,
    Values,
}

const NAME: &str = "Headers";
const ROOT: &str = "headers";
const PROTOTYPE: &str = "headers.prototype";
const ITERATOR: &str = "headersIterator.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
const NEXT: IntrinsicKey = IntrinsicKey::new("HeadersIterator", Slot::Prototype, "next");
const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
    RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    RootSpec::new(ITERATOR, RootKind::Plain, Some(SYNC_ROOT)),
];

macro_rules! methods {
    ($($method:ident => $name:literal / $length:literal),+ $(,)?) => {
        #[derive(Clone, Copy)]
        enum Method { $($method),+ }
        const METHODS: &[(&str, IntrinsicKey)] = &[
            $(($name, IntrinsicKey::new(NAME, Slot::Prototype, $name))),+
        ];
        const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor(CONSTRUCTOR, 0,
                |vm, _, roots, _, _| vm.throw_type_error(roots, "Headers requires new"),
                |vm, host, roots, args, prototype| vm.construct_headers(host, roots, &args, prototype),
            ),
            IntrinsicDef::call(NEXT, 0, |vm, _, roots, _, receiver| vm.headers_next(roots, receiver)),
            $(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, $name), $length,
                |vm, host, roots, args, receiver| {
                    vm.headers_method(host, roots, receiver, &args, Method::$method, $length)
                },
            )),+
        ];
    };
}

methods! {
    Append => "append" / 2,
    Delete => "delete" / 1,
    Get => "get" / 1,
    GetSetCookie => "getSetCookie" / 0,
    Has => "has" / 1,
    Set => "set" / 2,
    Entries => "entries" / 0,
    Keys => "keys" / 0,
    Values => "values" / 0,
    ForEach => "forEach" / 1,
}

pub(crate) const HEADERS_OWNER: Owner = Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));

fn install(vm: &mut VmCore, _: &Value) {
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, vm.builtins.get(ROOT), prototype);
    for &(name, key) in METHODS {
        let method = Value::object(vm.alloc_intrinsic(key));
        if name == "entries" {
            define_builtin_data(
                vm,
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                method.clone(),
                Attributes::METHOD,
            );
        }
        define_builtin_data(vm, prototype, name, method, Attributes::ORDINARY);
    }
    let iterator = vm.builtins.get(ITERATOR);
    let next = Value::object(vm.alloc_intrinsic(NEXT));
    define_builtin_data(vm, iterator, "next", next, Attributes::ORDINARY);
    for (target, tag) in [(prototype, NAME), (iterator, "Headers Iterator")] {
        define_builtin_data(
            vm,
            target,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(tag.into()),
            Attributes::FUNCTION_METADATA,
        );
    }
}

pub(crate) fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn http_whitespace(byte: &u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

pub(crate) fn valid_normalized_value(value: &[u8]) -> bool {
    !value.first().is_some_and(http_whitespace)
        && !value.last().is_some_and(http_whitespace)
        && !value.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
}

fn normalize(value: &mut Vec<u8>) {
    value.truncate(
        value
            .iter()
            .rposition(|b| !http_whitespace(b))
            .map_or(0, |i| i + 1),
    );
    let start = value.iter().position(|b| !http_whitespace(b)).unwrap_or(0);
    value.drain(..start);
}

fn trim_tab_space(value: &[u8]) -> &[u8] {
    let is_space = |byte: &u8| matches!(byte, b' ' | b'\t');
    let start = value
        .iter()
        .position(|b| !is_space(b))
        .unwrap_or(value.len());
    let value = &value[start..];
    &value[..value
        .iter()
        .rposition(|b| !is_space(b))
        .map_or(0, |i| i + 1)]
}

fn forbidden_request(name: &[u8], value: &[u8]) -> bool {
    if matches!(
        name,
        b"accept-charset"
            | b"accept-encoding"
            | b"access-control-request-headers"
            | b"access-control-request-method"
            | b"connection"
            | b"content-length"
            | b"cookie"
            | b"cookie2"
            | b"date"
            | b"dnt"
            | b"expect"
            | b"host"
            | b"keep-alive"
            | b"origin"
            | b"referer"
            | b"set-cookie"
            | b"te"
            | b"trailer"
            | b"transfer-encoding"
            | b"upgrade"
            | b"via"
    ) || name.starts_with(b"proxy-")
        || name.starts_with(b"sec-")
    {
        return true;
    }
    if !matches!(
        name,
        b"x-http-method" | b"x-http-method-override" | b"x-method-override"
    ) {
        return false;
    }
    // Commas inside HTTP quoted strings do not separate values.
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, &byte) in value.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b',' {
            if forbidden_method(&value[start..index]) {
                return true;
            }
            start = index + 1;
        }
    }
    forbidden_method(&value[start..])
}

fn forbidden_method(value: &[u8]) -> bool {
    let method = trim_tab_space(value);
    [b"CONNECT".as_slice(), b"TRACE", b"TRACK"]
        .iter()
        .any(|name| method.eq_ignore_ascii_case(name))
}

fn no_cors_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"accept" | b"accept-language" | b"content-language" | b"content-type"
    )
}

fn no_cors_safe(name: &[u8], value: &[u8]) -> bool {
    if value.len() > 128 {
        return false;
    }
    let unsafe_byte = |&b: &u8| (b < 0x20 && b != b'\t') || b"\"():<>?@[\\]{}\x7f".contains(&b);
    match name {
        b"accept" => !value.iter().any(unsafe_byte),
        b"accept-language" | b"content-language" => value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b" *,-.;=".contains(b)),
        b"content-type" => {
            if value.iter().any(unsafe_byte) {
                return false;
            }
            let essence = trim_tab_space(value.split(|&b| b == b';').next().unwrap_or_default());
            [
                b"application/x-www-form-urlencoded".as_slice(),
                b"multipart/form-data",
                b"text/plain",
            ]
            .iter()
            .any(|mime| essence.eq_ignore_ascii_case(mime))
        }
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum Mutation {
    Append,
    Set,
    Delete,
}

impl HeadersRecord {
    pub(crate) fn new(guard: HeadersGuard) -> Self {
        Self {
            list: Vec::new(),
            guard,
        }
    }

    pub(crate) fn get(&self, name: &[u8]) -> Option<Vec<u8>> {
        let mut values = self
            .list
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let mut combined = values.next()?.1.clone();
        for (_, value) in values {
            combined.extend_from_slice(b", ");
            combined.extend_from_slice(value);
        }
        Some(combined)
    }

    fn mutate(
        &mut self,
        method: Mutation,
        mut name: Vec<u8>,
        mut value: Vec<u8>,
    ) -> Result<(), &'static str> {
        normalize(&mut value);
        if !valid_name(&name) || !valid_normalized_value(&value) {
            return Err("invalid header name or value");
        }
        name.make_ascii_lowercase();
        match self.guard {
            HeadersGuard::Immutable => return Err("Headers are immutable"),
            HeadersGuard::Request if forbidden_request(&name, &value) => return Ok(()),
            HeadersGuard::Response if matches!(name.as_slice(), b"set-cookie" | b"set-cookie2") => {
                return Ok(());
            }
            HeadersGuard::RequestNoCors => {
                let allowed = match method {
                    Mutation::Delete => no_cors_name(&name) || name == b"range",
                    Mutation::Set => no_cors_safe(&name, &value),
                    Mutation::Append => match self.get(&name) {
                        None => no_cors_safe(&name, &value),
                        Some(mut combined) => {
                            combined.extend_from_slice(b", ");
                            combined.extend_from_slice(&value);
                            no_cors_safe(&name, &combined)
                        }
                    },
                };
                if !allowed {
                    return Ok(());
                }
            }
            _ => {}
        }
        match method {
            Mutation::Append => self.list.push((name, value)),
            Mutation::Set => {
                let mut replacement = Some(value);
                self.list.retain_mut(|(key, value)| {
                    if !key.eq_ignore_ascii_case(&name) {
                        true
                    } else if let Some(next) = replacement.take() {
                        *value = next;
                        true
                    } else {
                        false
                    }
                });
                if let Some(value) = replacement {
                    self.list.push((name, value));
                }
            }
            Mutation::Delete => {
                let previous = self.list.len();
                self.list
                    .retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
                if self.list.len() == previous {
                    return Ok(());
                }
            }
        }
        if self.guard == HeadersGuard::RequestNoCors {
            self.list
                .retain(|(key, _)| !key.eq_ignore_ascii_case(b"range"));
        }
        Ok(())
    }

    fn sorted_pair(&self, index: usize) -> Option<(&[u8], Vec<u8>)> {
        // Find the name at this index without copying values from unrelated entries.
        let mut counts = BTreeMap::<&[u8], usize>::new();
        for (name, _) in &self.list {
            let count = counts.entry(name).or_default();
            if name == b"set-cookie" || *count == 0 {
                *count += 1;
            }
        }
        let mut offset = index;
        for (name, count) in counts {
            if offset < count {
                let value = if name == b"set-cookie" {
                    self.list
                        .iter()
                        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                        .nth(offset)?
                        .1
                        .clone()
                } else {
                    self.get(name)?
                };
                return Some((name, value));
            }
            offset -= count;
        }
        None
    }
}

fn byte_string(bytes: &[u8]) -> Value {
    Value::string(JsString::from_units(
        bytes.iter().map(|&byte| u16::from(byte)).collect(),
    ))
}

macro_rules! convert {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

macro_rules! complete {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
    };
}

impl VmCore {
    pub(crate) fn alloc_headers(&mut self, data: HeadersRecord) -> ObjectId {
        self.heap.alloc(Object::Headers {
            object: PlainObject::new(Some(self.builtins.get(PROTOTYPE))),
            data,
        })
    }

    fn headers_bytes(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<Vec<u8>> {
        let string = convert!(self.coerce_string(host, roots, value));
        self.headers_encode(roots, &string)
    }

    fn headers_encode(
        &mut self,
        roots: &ActiveRoots<'_>,
        string: &JsString,
    ) -> Completion<Vec<u8>> {
        match string.units().map(u8::try_from).collect() {
            Ok(bytes) => Ok(Ok(bytes)),
            Err(_) => Ok(Err(self.type_error(
                roots,
                "header ByteString contains a code unit above 255",
            )?)),
        }
    }

    fn construct_headers(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get(PROTOTYPE));
        let roots = &roots.with(&prototype);
        let undefined = Value::UNDEFINED;
        let init = args.first().unwrap_or(&undefined);
        let data = complete!(self.headers_from_init(host, roots, init, HeadersGuard::None));
        let id = self.heap.alloc(Object::Headers {
            object: PlainObject::new(Some(prototype)),
            data,
        });
        Ok(Invocation::Value(Value::object(id)))
    }

    pub(crate) fn headers_from_init(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        init: &Value,
        guard: HeadersGuard,
    ) -> Completion<HeadersRecord> {
        let mut data = HeadersRecord::new(guard);
        let roots = &roots.with(init);
        let pairs = match init.view() {
            ValueRef::Undefined => return Ok(Ok(data)),
            ValueRef::Object(id) => {
                let method = match self.read_property(
                    host,
                    roots,
                    init,
                    PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
                )? {
                    Invocation::Value(method) => method,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
                if matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
                    convert!(self.headers_from_record(host, roots, init, id))
                } else {
                    convert!(self.headers_from_sequence(host, &roots.with(&method), init, &method))
                }
            }
            _ => {
                return Ok(Err(
                    self.type_error(roots, "Headers init must be an object")?
                ));
            }
        };
        // The entire Web IDL conversion precedes the Fetch fill algorithm.
        for pair in pairs {
            let Some((name, value)) = pair else {
                return Ok(Err(
                    self.type_error(roots, "Headers entries must contain two items")?
                ));
            };
            if let Err(message) = data.mutate(Mutation::Append, name, value) {
                return Ok(Err(self.type_error(roots, message)?));
            }
        }
        Ok(Ok(data))
    }

    fn headers_from_record(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        init: &Value,
        id: ObjectId,
    ) -> Completion<Vec<Option<HeaderPair>>> {
        let keys = convert!(self.own_keys(host, roots, id, false));
        let roots = &roots.with(&keys);
        let mut pairs = Vec::new();
        for key in &keys {
            let key_ref = match key {
                PropertyKey::String(key) => PropertyKeyRef::JsString(key),
                PropertyKey::Symbol(symbol) => PropertyKeyRef::Symbol(*symbol),
            };
            if !convert!(self.own_descriptor(host, roots, id, key_ref))
                .is_some_and(|(_, attrs)| attrs.enumerable)
            {
                continue;
            }
            let PropertyKey::String(key) = key else {
                return Ok(Err(self.type_error(roots, crate::ops::SYMBOL_TO_STRING)?));
            };
            let name = convert!(self.headers_encode(roots, key));
            let value = match self.read_property(host, roots, init, key_ref)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let value = convert!(self.headers_bytes(host, &roots.with(&value), &value));
            pairs.push(Some((name, value)));
        }
        Ok(Ok(pairs))
    }

    fn headers_from_sequence(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        init: &Value,
        method: &Value,
    ) -> Completion<Vec<Option<HeaderPair>>> {
        let (iterator, next) = convert!(self.get_iterator_from_method(host, roots, init, method));
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        let mut pairs = Vec::new();
        while let Some(item) = convert!(self.iterator_step(host, roots, &iterator, &next)) {
            match self.headers_pair(host, &roots.with(&item), &item)? {
                Ok(pair) => pairs.push(pair),
                Err(error) => {
                    self.close_iterator(host, &roots.with(&error), &iterator)?;
                    return Ok(Err(error));
                }
            }
        }
        Ok(Ok(pairs))
    }

    fn headers_pair(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        item: &Value,
    ) -> Completion<Option<HeaderPair>> {
        if !matches!(item.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "Headers entry must be iterable")?
            ));
        }
        let (iterator, next) = convert!(self.get_iterator(host, roots, item));
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        let (mut name, mut value, mut extra) = (None, None, false);
        while let Some(item) = convert!(self.iterator_step(host, roots, &iterator, &next)) {
            let bytes = match self.headers_bytes(host, &roots.with(&item), &item)? {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.close_iterator(host, &roots.with(&error), &iterator)?;
                    return Ok(Err(error));
                }
            };
            if name.is_none() {
                name = Some(bytes);
            } else if value.is_none() {
                value = Some(bytes);
            } else {
                extra = true;
            }
        }
        Ok(Ok(if extra { None } else { name.zip(value) }))
    }

    fn headers_method(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        method: Method,
        required: usize,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "invalid Headers receiver");
        };
        if !matches!(self.heap.get(id)?, Object::Headers { .. }) {
            return self.throw_type_error(roots, "invalid Headers receiver");
        }
        if args.len() < required {
            return self.throw_type_error(roots, "not enough Headers arguments");
        }
        let roots = &roots.with(&id);
        match method {
            Method::Entries | Method::Keys | Method::Values => {
                let kind = match method {
                    Method::Entries => HeadersIteratorKind::Entries,
                    Method::Keys => HeadersIteratorKind::Keys,
                    _ => HeadersIteratorKind::Values,
                };
                let id = self.heap.alloc(Object::HeadersIterator {
                    object: PlainObject::new(Some(self.builtins.get(ITERATOR))),
                    headers: id,
                    index: 0,
                    kind,
                });
                return Ok(Invocation::Value(Value::object(id)));
            }
            Method::ForEach => return self.headers_for_each(host, roots, receiver, id, args),
            Method::GetSetCookie => {
                let Object::Headers { data, .. } = self.heap.get(id)? else {
                    unreachable!()
                };
                let values = data
                    .list
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(b"set-cookie"))
                    .map(|(_, value)| byte_string(value))
                    .collect();
                return Ok(Invocation::Value(Value::object(
                    self.heap.alloc_array(values),
                )));
            }
            _ => {}
        }
        let name = complete!(self.headers_bytes(host, roots, &args[0]));
        let value = if matches!(method, Method::Append | Method::Set) {
            complete!(self.headers_bytes(host, roots, &args[1]))
        } else {
            Vec::new()
        };
        if matches!(method, Method::Get | Method::Has) {
            if !valid_name(&name) {
                return self.throw_type_error(roots, "invalid header name");
            }
            let Object::Headers { data, .. } = self.heap.get(id)? else {
                unreachable!()
            };
            let value = if matches!(method, Method::Get) {
                data.get(&name)
                    .map_or(Value::NULL, |bytes| byte_string(&bytes))
            } else {
                Value::boolean(
                    data.list
                        .iter()
                        .any(|(key, _)| key.eq_ignore_ascii_case(&name)),
                )
            };
            return Ok(Invocation::Value(value));
        }
        let mutation = match method {
            Method::Append => Mutation::Append,
            Method::Set => Mutation::Set,
            Method::Delete => Mutation::Delete,
            _ => unreachable!(),
        };
        let Object::Headers { data, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        if let Err(message) = data.mutate(mutation, name, value) {
            return self.throw_type_error(roots, message);
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn headers_for_each(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let callback = &args[0];
        if !self.is_callable(callback) {
            return self.throw_type_error(roots, "Headers.forEach callback must be callable");
        }
        let undefined = Value::UNDEFINED;
        let this_arg = args.get(1).unwrap_or(&undefined);
        for index in 0.. {
            let Object::Headers { data, .. } = self.heap.get(id)? else {
                unreachable!()
            };
            let Some((name, value)) = data.sorted_pair(index) else {
                break;
            };
            let arguments = [byte_string(&value), byte_string(name), receiver.clone()];
            if let thrown @ Invocation::Thrown(_) = self.invoke(
                host,
                roots,
                callback,
                Arguments::Values(&arguments),
                this_arg,
            )? {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn headers_next(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "invalid Headers iterator receiver");
        };
        let Object::HeadersIterator {
            headers,
            index,
            kind,
            ..
        } = *self.heap.get(id)?
        else {
            return self.throw_type_error(roots, "invalid Headers iterator receiver");
        };
        let Object::Headers { data, .. } = self.heap.get(headers)? else {
            unreachable!()
        };
        if let Some((name, value)) = data.sorted_pair(index) {
            let value = match kind {
                HeadersIteratorKind::Entries => {
                    let pair = vec![byte_string(name), byte_string(&value)];
                    Value::object(self.heap.alloc_array(pair))
                }
                HeadersIteratorKind::Keys => byte_string(name),
                HeadersIteratorKind::Values => byte_string(&value),
            };
            let Object::HeadersIterator { index, .. } = self.heap.get_mut(id)? else {
                unreachable!()
            };
            *index += 1;
            return self.iterator_result(value, false);
        }
        self.iterator_result(Value::UNDEFINED, true)
    }
}
