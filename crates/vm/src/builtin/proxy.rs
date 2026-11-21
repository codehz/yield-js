//! Proxy internal methods. Traps may mutate the target, so invariants are
//! checked against descriptors obtained after calling the trap.

use crate::builtin::globals::Owner;
use crate::builtin::{ROOT_FUNCTION_PROTOTYPE, ROOT_PROXY, RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Object, OwnProperty, PlainObject, Property};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops;
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Proxy", Slot::Global, "");

pub(crate) const PROXY_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::constructor(
    CONSTRUCTOR,
    2,
    |vm, _, _, _, _| vm.throw_type_error("Proxy requires new"),
    |vm, _, _, args, _| vm.construct_proxy(&args),
)
.without_new_target_prototype()];

pub(crate) const OWNER: Owner = Owner::new(ROOT_PROXY, PROXY_ROOTS, &[PROXY_INTRINSICS], None);

pub(crate) const PROXY_ROOTS: &[RootSpec] = &[RootSpec::new(
    ROOT_PROXY,
    RootKind::Native(CONSTRUCTOR),
    Some(ROOT_FUNCTION_PROTOTYPE),
)];

macro_rules! unwrap_script {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

impl Vm {
    fn construct_proxy(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return self.throw_type_error("Proxy requires an object target");
        };
        let Some(ValueRef::Object(handler)) = args.get(1).map(Value::view) else {
            return self.throw_type_error("Proxy requires an object handler");
        };
        let base = self.heap.proxy_target(target)?;
        if !matches!(
            self.heap.get(base)?,
            Object::Plain(_) | Object::Array(_) | Object::Boxed { .. } | Object::Date { .. }
        ) {
            return self.throw_type_error(
                "Proxy supports only plain object, array, primitive wrapper, and Date targets",
            );
        }
        Ok(Invocation::Value(Value::object(self.heap.alloc(
            Object::Proxy {
                target,
                handler,
                object: PlainObject::new(None),
            },
        ))))
    }

    fn proxy_trap(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        handler: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let handler = Value::object(handler);
        let trap = self.read_property(
            host,
            &roots.with(&(&handler, &args)),
            &handler,
            PropertyKeyRef::String(name),
        )?;
        match trap {
            Invocation::Value(value)
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
            {
                Ok(Ok(None))
            }
            Invocation::Value(method) => match self.invoke(host, roots, &method, args, &handler)? {
                Invocation::Value(value) => Ok(Ok(Some(value))),
                Invocation::Thrown(error) => Ok(Err(error)),
            },
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn proxy_get(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, handler, key, receiver);
        let roots = &roots.with(&root_view);
        let result = self.proxy_trap(
            host,
            roots,
            handler,
            "get",
            vec![Value::object(target), key_value(key), receiver.clone()],
        )?;
        let value = match result {
            Ok(None) => {
                return self.read_property_with_receiver(host, roots, target, receiver, key);
            }
            Ok(Some(value)) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&value);
        match self.own_descriptor(host, roots, target, key)? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some((property, attrs))) if !attrs.configurable => {
                let valid = match property {
                    Property::Data(fixed) if !attrs.writable => ops::same_value(&fixed, &value),
                    Property::Accessor { get: None, .. } => value == Value::UNDEFINED,
                    _ => true,
                };
                if !valid {
                    return self.throw_type_error("Proxy get trap contradicts a fixed property");
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(value))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    pub(crate) fn proxy_set(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        proxy: (ObjectId, ObjectId),
        receiver: &Value,
        key: PropertyKey,
        value: Value,
        strict: bool,
    ) -> Result<Invocation, VmError> {
        let (target, handler) = proxy;
        let proxy_view = (proxy, receiver);
        let roots = &roots.with(&proxy_view);
        let inputs = (&key, &value);
        let trap_roots = &roots.with(&inputs);
        match self.proxy_trap(
            host,
            trap_roots,
            handler,
            "set",
            vec![
                Value::object(target),
                key_value(key.borrowed()),
                value.clone(),
                receiver.clone(),
            ],
        )? {
            Ok(None) => {
                return self.set_property(host, roots, target, receiver, key, value, strict);
            }
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(result)) if !result.truthy() => {
                return if strict {
                    self.throw_type_error("Proxy set trap rejected the write")
                } else {
                    Ok(Invocation::Value(Value::boolean(false)))
                };
            }
            _ => {}
        }
        match self.own_descriptor(host, trap_roots, target, key.borrowed())? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some((property, attrs))) if !attrs.configurable => {
                let valid = match property {
                    Property::Data(fixed) if !attrs.writable => ops::same_value(&fixed, &value),
                    Property::Accessor { set: None, .. } => false,
                    _ => true,
                };
                if !valid {
                    return self.throw_type_error("Proxy set trap contradicts a fixed property");
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(Value::boolean(true)))
    }

    pub(crate) fn proxy_presence(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        delete: bool,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, handler, key);
        let roots = &roots.with(&root_view);
        let name = if delete { "deleteProperty" } else { "has" };
        let flag = match self.proxy_trap(
            host,
            roots,
            handler,
            name,
            vec![Value::object(target), key_value(key)],
        )? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(value)) => value.truthy(),
            Ok(None) if delete => {
                return self.delete_property(host, roots, &Value::object(target), &key_value(key));
            }
            Ok(None) => return self.has_property_key(host, roots, target, key),
        };
        if flag == delete {
            let extensible = self
                .heap
                .get(self.heap.proxy_target(target)?)?
                .is_extensible();
            match self.own_descriptor(host, roots, target, key)? {
                Err(error) => return Ok(Invocation::Thrown(error)),
                Ok(Some((_, attrs))) if !attrs.configurable || !extensible => return self.throw_type_error("Proxy trap contradicts a protected own property"),
                Ok(None) if delete && !extensible => return self.throw_type_error("Proxy deleteProperty trap cannot report success for a missing property on a non-extensible target"),
                _ => {}
            }
        }
        Ok(Invocation::Value(Value::boolean(flag)))
    }

    pub(crate) fn own_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        include_metadata: bool,
    ) -> Result<Result<Vec<PropertyKey>, Value>, VmError> {
        let roots = &roots.with(&id);
        let Object::Proxy {
            target, handler, ..
        } = *self.heap.get(id)?
        else {
            return Ok(Ok(self.heap.get(id)?.own_keys(include_metadata).collect()));
        };
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "ownKeys",
            vec![Value::object(target)]
        )) else {
            return self.own_keys(host, roots, target, include_metadata);
        };
        if !matches!(result.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error("Proxy ownKeys trap must return an object")?
            ));
        }
        let root_view = (target, &result);
        let roots = &roots.with(&root_view);
        let length =
            match self.read_property(host, roots, &result, PropertyKeyRef::String("length"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        let mut keys = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let iterable = if length == Value::UNDEFINED {
            match self.read_property(
                host,
                roots,
                &result,
                PropertyKeyRef::Symbol(crate::heap::ITERATOR_SYMBOL),
            )? {
                Invocation::Value(value)
                    if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
                {
                    None
                }
                Invocation::Value(method) => Some(unwrap_script!(
                    self.get_iterator_from_method(host, roots, &result, &method)
                )),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            None
        };
        let roots = &roots.with(&iterable);
        let length = unwrap_script!(self.coerce_number(host, roots, &length))
            .clamp(0.0, 9_007_199_254_740_991.0) as u64;
        let mut index = 0;
        loop {
            let roots = &roots.with(&keys);
            let entry = if let Some((iterator, next)) = &iterable {
                let Some(value) = unwrap_script!(self.iterator_step(host, roots, iterator, next))
                else {
                    break;
                };
                Invocation::Value(value)
            } else {
                if index >= length {
                    break;
                }
                let entry = self.read_property(
                    host,
                    roots,
                    &result,
                    PropertyKeyRef::String(&index.to_string()),
                )?;
                index += 1;
                entry
            };
            let entry = match entry {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let key = if matches!(entry.view(), ValueRef::String(_)) {
                PropertyKey::String(entry.into_string().expect("ownKeys entry is a string"))
            } else if let ValueRef::Symbol(key) = entry.view() {
                PropertyKey::Symbol(key)
            } else {
                let error = self.type_error("Proxy ownKeys entries must be strings or symbols")?;
                if let Some((iterator, _)) = &iterable {
                    self.close_iterator(host, &roots.with(&error), iterator)?;
                }
                return Ok(Err(error));
            };
            if !seen.insert(key.clone()) {
                if let Some((iterator, _)) = &iterable {
                    self.close_iterator(host, roots, iterator)?;
                }
                return Ok(Err(
                    self.type_error("Proxy ownKeys trap returned a duplicate key")?
                ));
            }
            keys.push(key);
        }
        let extensible = self
            .heap
            .get(self.heap.proxy_target(target)?)?
            .is_extensible();
        let roots = &roots.with(&keys);
        let target_keys = unwrap_script!(self.own_keys(host, roots, target, false));
        if !extensible && target_keys.len() != keys.len() {
            return Ok(Err(self.type_error(
                "Proxy ownKeys trap changed keys of a non-extensible target",
            )?));
        }
        let roots = &roots.with(&target_keys);
        for key in &target_keys {
            let descriptor =
                unwrap_script!(self.own_descriptor(host, roots, target, key.borrowed()));
            if (!extensible || descriptor.is_some_and(|(_, attrs)| !attrs.configurable))
                && !seen.contains(key)
            {
                return Ok(Err(
                    self.type_error("Proxy ownKeys trap omitted a protected key")?
                ));
            }
        }
        Ok(Ok(keys))
    }

    pub(crate) fn enumerable_string_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<Vec<JsString>, Value>, VmError> {
        let roots = &roots.with(&id);
        let ordinary = !matches!(self.heap.get(id)?, Object::Proxy { .. });
        let keys = unwrap_script!(self.own_keys(host, roots, id, ordinary));
        let mut names = Vec::new();
        let roots = &roots.with(&keys);
        for key in &keys {
            if let PropertyKey::String(key) = key
                && (ordinary
                    || unwrap_script!(self.own_descriptor(
                        host,
                        roots,
                        id,
                        PropertyKeyRef::JsString(key)
                    ))
                    .is_some_and(|(_, attrs)| attrs.enumerable))
            {
                names.push(key.clone());
            }
        }
        Ok(Ok(names))
    }

    pub(crate) fn own_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Result<Option<(Property, Attributes)>, Value>, VmError> {
        let root_view = (id, key);
        let roots = &roots.with(&root_view);
        let Object::Proxy {
            target, handler, ..
        } = *self.heap.get(id)?
        else {
            let property = self.heap.get(id)?.own_property(key);
            return Ok(Ok(property.map(|property| {
                let attrs = property.attributes();
                (property.into_owned(), attrs)
            })));
        };
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "getOwnPropertyDescriptor",
            vec![Value::object(target), key_value(key)]
        )) else {
            return self.own_descriptor(host, roots, target, key);
        };
        if !matches!(result.view(), ValueRef::Undefined | ValueRef::Object(_)) {
            return Ok(Err(self.type_error(
                "Proxy descriptor trap must return an object or undefined",
            )?));
        }
        let roots = &roots.with(&result);
        let current = unwrap_script!(self.own_descriptor(host, roots, target, key));
        let extensible = self
            .heap
            .get(self.heap.proxy_target(target)?)?
            .is_extensible();
        let invalid = |vm: &mut Self| {
            Ok(Err(vm.type_error(
                "Proxy descriptor trap reported an incompatible descriptor",
            )?))
        };
        if result == Value::UNDEFINED {
            return if current.is_some_and(|(_, attrs)| !attrs.configurable || !extensible) {
                invalid(self)
            } else {
                Ok(Ok(None))
            };
        }
        let current_property = current.as_ref().map(|(property, _)| property);
        let update = unwrap_script!(self.read_property_descriptor(
            host,
            &roots.with(&current_property),
            &result
        ));
        let Some((property, attrs)) = update.complete(true, None) else {
            unreachable!()
        };
        if !attrs.configurable
            && current
                .as_ref()
                .is_none_or(|(_, old)| old.configurable || !attrs.writable && old.writable)
        {
            return invalid(self);
        }
        let mut complete = DescriptorUpdate {
            enumerable: Some(attrs.enumerable),
            configurable: Some(attrs.configurable),
            ..DescriptorUpdate::default()
        };
        match &property {
            Property::Data(value) => {
                complete.value = Some(value.clone());
                complete.writable = Some(attrs.writable);
            }
            Property::Accessor { get, set } => {
                complete.get = Some(*get);
                complete.set = Some(*set);
            }
        }
        if complete
            .complete(
                extensible,
                current
                    .as_ref()
                    .map(|(property, attrs)| OwnProperty::Stored(property, *attrs)),
            )
            .is_none()
        {
            return invalid(self);
        }
        Ok(Ok(Some((property, attrs))))
    }

    pub(crate) fn for_in_next(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterator: ObjectId,
    ) -> Result<Result<Option<JsString>, Value>, VmError> {
        let roots = &roots.with(&iterator);
        loop {
            let Object::ForInIterator {
                current,
                pending,
                visited,
                string_indices,
            } = self.heap.get_mut(iterator)?
            else {
                return Err(VmError::InvalidState("expected a for-in cursor".into()));
            };
            if let Some(index) = string_indices.next() {
                return Ok(Ok(Some(index.to_string().into())));
            }
            let Some(id) = *current else {
                return Ok(Ok(None));
            };
            if let Some(keys) = pending {
                if let Some(key) = keys.pop_front() {
                    if visited.contains(&key)
                        || ops::canonical_index_key(PropertyKeyRef::JsString(&key))
                            .is_some_and(|index| index < string_indices.end)
                    {
                        continue;
                    }
                    let Some((_, attrs)) = unwrap_script!(self.own_descriptor(
                        host,
                        roots,
                        id,
                        PropertyKeyRef::JsString(&key)
                    )) else {
                        continue;
                    };
                    let Object::ForInIterator { visited, .. } = self.heap.get_mut(iterator)? else {
                        unreachable!()
                    };
                    if attrs.enumerable {
                        visited.insert(key.clone());
                        return Ok(Ok(Some(key)));
                    }
                    visited.insert(key);
                } else {
                    let prototype = self.heap.prototype_of(id)?;
                    let Object::ForInIterator {
                        current, pending, ..
                    } = self.heap.get_mut(iterator)?
                    else {
                        unreachable!()
                    };
                    *current = prototype;
                    *pending = None;
                }
            } else {
                // Snapshot each level's keys on first reaching it, but recheck
                // its descriptors as keys are visited (including after await).
                let keys = unwrap_script!(self.own_keys(host, roots, id, false))
                    .into_iter()
                    .filter_map(|key| match key {
                        PropertyKey::String(key) => Some(key),
                        PropertyKey::Symbol(_) => None,
                    })
                    .collect();
                let Object::ForInIterator { pending, .. } = self.heap.get_mut(iterator)? else {
                    unreachable!()
                };
                *pending = Some(keys);
            }
        }
    }
}

fn key_value(key: PropertyKeyRef<'_>) -> Value {
    match key {
        PropertyKeyRef::String(key) => Value::string(key.into()),
        PropertyKeyRef::JsString(key) => Value::string(key.clone()),
        PropertyKeyRef::Symbol(key) => Value::symbol(key),
    }
}
