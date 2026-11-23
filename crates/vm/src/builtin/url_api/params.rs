//! URLSearchParams keeps an ordered list of pairs; iterators read that list by index.

use std::collections::BTreeMap;

use super::{parse_query, serialize_query};
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data,
    globals::Owner,
    iterator::SYNC_ROOT,
    link_constructor,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DescriptorUpdate, ITERATOR_SYMBOL, Object, PlainObject, TO_STRING_TAG_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const NAME: &str = "URLSearchParams";
const ROOT: &str = "urlSearchParams";
pub(super) const PROTOTYPE: &str = "urlSearchParams.prototype";
const ITERATOR: &str = "urlSearchParamsIterator.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
const SIZE: IntrinsicKey = IntrinsicKey::new(NAME, Slot::PrototypeGetter, "size");
const NEXT: IntrinsicKey = IntrinsicKey::new("URLSearchParamsIterator", Slot::Prototype, "next");

const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
    RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    RootSpec::new(ITERATOR, RootKind::Plain, Some(SYNC_ROOT)),
];

#[derive(Clone, Copy)]
enum Method {
    Append,
    Delete,
    Get,
    GetAll,
    Has,
    Set,
    Sort,
    Stringify,
    Entries,
    Keys,
    Values,
    ForEach,
    Size,
}

macro_rules! methods {
    ($($method:ident => $name:literal / $length:literal),+ $(,)?) => {
        const METHODS: &[(&str, IntrinsicKey)] = &[
            $(($name, IntrinsicKey::new(NAME, Slot::Prototype, $name))),+
        ];
        const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor(CONSTRUCTOR, 0,
                |vm, _, roots, _, _| vm.throw_type_error(roots, "URLSearchParams requires new"),
                |vm, host, roots, args, prototype| vm.construct_search_params(host, roots, &args, prototype),
            ),
            IntrinsicDef::call(SIZE, 0, |vm, host, roots, _, receiver| {
                vm.search_params_method(host, roots, receiver, &[], Method::Size, 0)
            }),
            IntrinsicDef::call(NEXT, 0, |vm, _, roots, _, receiver| vm.search_params_next(roots, receiver)),
            $(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, $name), $length,
                |vm, host, roots, args, receiver| {
                    vm.search_params_method(host, roots, receiver, &args, Method::$method, $length)
                },
            )),+
        ];
    };
}

methods! {
    Append => "append" / 2,
    Delete => "delete" / 1,
    Get => "get" / 1,
    GetAll => "getAll" / 1,
    Has => "has" / 1,
    Set => "set" / 2,
    Sort => "sort" / 0,
    Stringify => "toString" / 0,
    Entries => "entries" / 0,
    Keys => "keys" / 0,
    Values => "values" / 0,
    ForEach => "forEach" / 1,
}

pub(crate) const PARAMS_OWNER: Owner = Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));

fn install(vm: &mut Vm, _: &Value) {
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
    let getter = vm.alloc_intrinsic(SIZE);
    assert!(
        vm.heap
            .apply_descriptor(
                prototype,
                "size".into(),
                DescriptorUpdate {
                    get: Some(Some(getter)),
                    enumerable: Some(true),
                    configurable: Some(true),
                    ..DescriptorUpdate::default()
                }
            )
            .expect("URLSearchParams size getter")
    );
    let iterator = vm.builtins.get(ITERATOR);
    let next = Value::object(vm.alloc_intrinsic(NEXT));
    define_builtin_data(vm, iterator, "next", next, Attributes::ORDINARY);
    for (target, tag) in [(prototype, NAME), (iterator, "URLSearchParams Iterator")] {
        define_builtin_data(
            vm,
            target,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(tag.into()),
            Attributes::FUNCTION_METADATA,
        );
    }
}

type Completion<T> = Result<Result<T, Value>, VmError>;
type Pairs = Vec<(String, String)>;

fn usv_string(string: &JsString) -> String {
    char::decode_utf16(string.units())
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn string_value(string: &str) -> Value {
    Value::string(string.into())
}

macro_rules! convert {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

impl Vm {
    fn params_usv(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<String> {
        Ok(self
            .coerce_string(host, roots, value)?
            .map(|string| usv_string(&string)))
    }

    fn construct_search_params(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&prototype);
        let undefined = Value::UNDEFINED;
        let init = args.first().unwrap_or(&undefined);
        let pairs = match init.view() {
            ValueRef::Undefined => Vec::new(),
            ValueRef::Object(id) => {
                let method = match self.read_property(
                    host,
                    roots,
                    init,
                    PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
                )? {
                    Invocation::Value(method) => method,
                    thrown => return Ok(thrown),
                };
                let pairs = if matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
                    self.params_from_record(host, roots, init, id)?
                } else {
                    self.params_from_sequence(host, roots, init, &method)?
                };
                match pairs {
                    Ok(pairs) => pairs,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => match self.params_usv(host, roots, init)? {
                Ok(query) => parse_query(query.strip_prefix('?').unwrap_or(&query)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let prototype = prototype.unwrap_or_else(|| self.builtins.get(PROTOTYPE));
        let id = self.heap.alloc(Object::UrlSearchParams {
            object: PlainObject::new(Some(prototype)),
            pairs,
            owner: None,
        });
        Ok(Invocation::Value(Value::object(id)))
    }

    fn params_from_record(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        init: &Value,
        id: ObjectId,
    ) -> Completion<Pairs> {
        let keys = convert!(self.own_keys(host, roots, id, true));
        let roots = &roots.with(&keys);
        let mut pairs: Pairs = Vec::new();
        let mut indices = BTreeMap::new();
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
            let key = usv_string(key);
            let value = match self.read_property(host, roots, init, key_ref)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let value = convert!(self.params_usv(host, roots, &value));
            match indices.entry(key.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => {
                    let index: usize = *entry.get();
                    pairs[index].1 = value;
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(pairs.len());
                    pairs.push((key, value));
                }
            }
        }
        Ok(Ok(pairs))
    }

    fn params_from_sequence(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        init: &Value,
        method: &Value,
    ) -> Completion<Pairs> {
        let (iterator, next) = convert!(self.get_iterator_from_method(host, roots, init, method));
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        let mut pairs = Vec::new();
        let mut invalid_length = false;
        while let Some(item) = convert!(self.iterator_step(host, roots, &iterator, &next)) {
            match self.params_pair(host, roots, &item)? {
                Ok(Some(pair)) => pairs.push(pair),
                Ok(None) => invalid_length = true,
                Err(error) => {
                    self.close_iterator(host, &roots.with(&error), &iterator)?;
                    return Ok(Err(error));
                }
            }
        }
        if invalid_length {
            return Ok(Err(self.type_error(
                roots,
                "URLSearchParams entries must contain two items",
            )?));
        }
        Ok(Ok(pairs))
    }

    fn params_pair(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        item: &Value,
    ) -> Completion<Option<(String, String)>> {
        if !matches!(item.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "URLSearchParams entry must be iterable")?
            ));
        }
        let (iterator, next) = convert!(self.get_iterator(host, roots, item));
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        let mut name = None;
        let mut value = None;
        let mut extra = false;
        while let Some(item) = convert!(self.iterator_step(host, roots, &iterator, &next)) {
            let string = match self.params_usv(host, roots, &item)? {
                Ok(string) => string,
                Err(error) => {
                    self.close_iterator(host, &roots.with(&error), &iterator)?;
                    return Ok(Err(error));
                }
            };
            if name.is_none() {
                name = Some(string);
            } else if value.is_none() {
                value = Some(string);
            } else {
                extra = true;
            }
        }
        Ok(Ok(if extra { None } else { name.zip(value) }))
    }

    fn search_params_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        method: Method,
        required: usize,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(
                roots,
                "URLSearchParams method requires a URLSearchParams receiver",
            );
        };
        if !matches!(self.heap.get(id)?, Object::UrlSearchParams { .. }) {
            return self.throw_type_error(
                roots,
                "URLSearchParams method requires a URLSearchParams receiver",
            );
        }
        if args.len() < required {
            return self.throw_type_error(roots, "not enough URLSearchParams arguments");
        }
        let roots = &roots.with(&id);
        match method {
            Method::Entries | Method::Keys | Method::Values => {
                let kind = match method {
                    Method::Entries => 0,
                    Method::Keys => 1,
                    Method::Values => 2,
                    _ => unreachable!(),
                };
                let iterator = self.heap.alloc(Object::UrlSearchParamsIterator {
                    object: PlainObject::new(Some(self.builtins.get(ITERATOR))),
                    params: id,
                    index: 0,
                    kind,
                });
                return Ok(Invocation::Value(Value::object(iterator)));
            }
            Method::ForEach => return self.search_params_for_each(host, roots, receiver, id, args),
            _ => {}
        }
        let name = if required > 0 {
            match self.params_usv(host, roots, &args[0])? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        } else {
            String::new()
        };
        let value = match method {
            Method::Append | Method::Set => Some(&args[1]),
            Method::Has | Method::Delete => args
                .get(1)
                .filter(|value| !matches!(value.view(), ValueRef::Undefined)),
            _ => None,
        };
        let value = match value {
            Some(value) => match self.params_usv(host, roots, value)? {
                Ok(string) => Some(string),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            None => None,
        };
        let Object::UrlSearchParams { pairs, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        let result = match method {
            Method::Append => {
                pairs.push((name, value.expect("converted second argument")));
                None
            }
            Method::Delete => {
                pairs.retain(|(key, text)| {
                    *key != name || value.as_ref().is_some_and(|value| value != text)
                });
                None
            }
            Method::Get => Some(
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map_or(Value::NULL, |(_, text)| string_value(text)),
            ),
            Method::GetAll => {
                let values = pairs
                    .iter()
                    .filter(|(key, _)| *key == name)
                    .map(|(_, text)| string_value(text))
                    .collect();
                Some(Value::object(self.heap.alloc_array(values)))
            }
            Method::Has => Some(Value::boolean(pairs.iter().any(|(key, text)| {
                *key == name && value.as_ref().is_none_or(|value| value == text)
            }))),
            Method::Set => {
                let mut replacement = value;
                pairs.retain_mut(|(key, text)| {
                    if *key != name {
                        return true;
                    }
                    if let Some(value) = replacement.take() {
                        *text = value;
                        true
                    } else {
                        false
                    }
                });
                if let Some(value) = replacement {
                    pairs.push((name, value));
                }
                None
            }
            Method::Sort => {
                pairs
                    .sort_by(|(left, _), (right, _)| left.encode_utf16().cmp(right.encode_utf16()));
                None
            }
            Method::Stringify => Some(Value::string(serialize_query(pairs).into())),
            Method::Size => Some(Value::number(pairs.len() as f64)),
            _ => unreachable!(),
        };
        if let Some(result) = result {
            return Ok(Invocation::Value(result));
        }
        self.sync_url_query(id)?;
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn search_params_for_each(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let callback = &args[0];
        if !self.is_callable(callback) {
            return self
                .throw_type_error(roots, "URLSearchParams.forEach callback must be callable");
        }
        let undefined = Value::UNDEFINED;
        let this_arg = args.get(1).unwrap_or(&undefined);
        for index in 0.. {
            let Object::UrlSearchParams { pairs, .. } = self.heap.get(id)? else {
                unreachable!()
            };
            let Some((name, value)) = pairs.get(index) else {
                break;
            };
            let call_args = [string_value(value), string_value(name), receiver.clone()];
            if let thrown @ Invocation::Thrown(_) = self.invoke(
                host,
                roots,
                callback,
                Arguments::Values(&call_args),
                this_arg,
            )? {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn search_params_next(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid_receiver = "URLSearchParams iterator next requires a URLSearchParams iterator";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::UrlSearchParamsIterator {
            params,
            index,
            kind,
            ..
        } = *self.heap.get(id)?
        else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::UrlSearchParams { pairs, .. } = self.heap.get(params)? else {
            unreachable!()
        };
        let Some((name, value)) = pairs.get(index) else {
            return self.iterator_result(Value::UNDEFINED, true);
        };
        let value = match kind {
            0 => {
                let pair = vec![string_value(name), string_value(value)];
                Value::object(self.heap.alloc_array(pair))
            }
            1 => string_value(name),
            2 => string_value(value),
            _ => unreachable!(),
        };
        let Object::UrlSearchParamsIterator { index, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        *index += 1;
        self.iterator_result(value, false)
    }
}
