//! Proxy internal methods. Traps may mutate the target, so invariants are
//! checked against descriptors obtained after calling the trap.

use crate::error::VmError;
use crate::heap::{Attributes, DescriptorUpdate, Object, OwnProperty, Property};
use crate::host::Host;
use crate::ops;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

macro_rules! unwrap_script {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

impl Vm {
    fn proxy_trap(
        &mut self,
        host: &mut dyn Host,
        handler: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let handler = Value::Object(handler);
        match self.read_property(host, &handler, PropertyKeyRef::String(name))? {
            Invocation::Value(Value::Undefined | Value::Null) => Ok(Ok(None)),
            Invocation::Value(method) => match self.invoke(host, &method, args, &handler)? {
                Invocation::Value(value) => Ok(Ok(Some(value))),
                Invocation::Thrown(error) => Ok(Err(error)),
            },
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn proxy_get(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let result = self.proxy_trap(
            host,
            handler,
            "get",
            vec![Value::Object(target), key_value(key), receiver.clone()],
        )?;
        let value = match result {
            Ok(None) => return self.read_property_with_receiver(host, target, receiver, key),
            Ok(Some(value)) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match self.own_descriptor(host, target, key)? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some((property, attrs))) if !attrs.configurable => {
                let valid = match property {
                    Property::Data(fixed) if !attrs.writable => ops::same_value(&fixed, &value),
                    Property::Accessor { get: None, .. } => value == Value::Undefined,
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

    pub(crate) fn proxy_set(
        &mut self,
        host: &mut dyn Host,
        proxy: (ObjectId, ObjectId),
        receiver: &Value,
        key: PropertyKey,
        value: Value,
        strict: bool,
    ) -> Result<Invocation, VmError> {
        let (target, handler) = proxy;
        match self.proxy_trap(
            host,
            handler,
            "set",
            vec![
                Value::Object(target),
                key_value(key.borrowed()),
                value.clone(),
                receiver.clone(),
            ],
        )? {
            Ok(None) => return self.set_property(host, target, receiver, key, value, strict),
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(result)) if !result.truthy() => {
                return if strict {
                    self.throw_type_error("Proxy set trap rejected the write")
                } else {
                    Ok(Invocation::Value(Value::Bool(false)))
                };
            }
            _ => {}
        }
        match self.own_descriptor(host, target, key.borrowed())? {
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
        Ok(Invocation::Value(Value::Bool(true)))
    }

    pub(crate) fn proxy_presence(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        delete: bool,
    ) -> Result<Invocation, VmError> {
        let name = if delete { "deleteProperty" } else { "has" };
        let flag = match self.proxy_trap(
            host,
            handler,
            name,
            vec![Value::Object(target), key_value(key)],
        )? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(value)) => value.truthy(),
            Ok(None) if delete => {
                return self.delete_property(host, &Value::Object(target), &key_value(key));
            }
            Ok(None) => return self.has_property_key(host, target, key),
        };
        if flag == delete {
            let extensible = self
                .heap
                .get(self.heap.proxy_target(target)?)?
                .is_extensible();
            match self.own_descriptor(host, target, key)? {
                Err(error) => return Ok(Invocation::Thrown(error)),
                Ok(Some((_, attrs))) if !attrs.configurable || !extensible => return self.throw_type_error("Proxy trap contradicts a protected own property"),
                Ok(None) if delete && !extensible => return self.throw_type_error("Proxy deleteProperty trap cannot report success for a missing property on a non-extensible target"),
                _ => {}
            }
        }
        Ok(Invocation::Value(Value::Bool(flag)))
    }

    pub(crate) fn own_keys(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        include_metadata: bool,
    ) -> Result<Result<Vec<PropertyKey>, Value>, VmError> {
        let Object::Proxy {
            target, handler, ..
        } = *self.heap.get(id)?
        else {
            return Ok(Ok(self.heap.get(id)?.own_keys(include_metadata).collect()));
        };
        let Some(result) =
            unwrap_script!(self.proxy_trap(host, handler, "ownKeys", vec![Value::Object(target)]))
        else {
            return self.own_keys(host, target, include_metadata);
        };
        if !matches!(result, Value::Object(_)) {
            return Ok(Err(
                self.type_error("Proxy ownKeys trap must return an object")?
            ));
        }
        let length = match self.read_property(host, &result, PropertyKeyRef::String("length"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let mut keys = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let iterable = if length == Value::Undefined {
            match self.read_property(
                host,
                &result,
                PropertyKeyRef::Symbol(crate::heap::ITERATOR_SYMBOL),
            )? {
                Invocation::Value(Value::Undefined | Value::Null) => None,
                Invocation::Value(method) => Some(unwrap_script!(
                    self.get_iterator_from_method(host, &result, &method)
                )),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            None
        };
        let length = unwrap_script!(self.coerce_number(host, &length))
            .clamp(0.0, 9_007_199_254_740_991.0) as u64;
        let mut index = 0;
        loop {
            let entry = if let Some((iterator, next)) = &iterable {
                let Some(value) = unwrap_script!(self.iterator_step(host, iterator, next)) else {
                    break;
                };
                Invocation::Value(value)
            } else {
                if index >= length {
                    break;
                }
                let entry =
                    self.read_property(host, &result, PropertyKeyRef::String(&index.to_string()))?;
                index += 1;
                entry
            };
            let key = match entry {
                Invocation::Value(Value::String(key)) => PropertyKey::String(key),
                Invocation::Value(Value::Symbol(key)) => PropertyKey::Symbol(key),
                Invocation::Thrown(error) => return Ok(Err(error)),
                Invocation::Value(_) => {
                    let error =
                        self.type_error("Proxy ownKeys entries must be strings or symbols")?;
                    if let Some((iterator, _)) = &iterable {
                        self.close_iterator(host, iterator)?;
                    }
                    return Ok(Err(error));
                }
            };
            if !seen.insert(key.clone()) {
                if let Some((iterator, _)) = &iterable {
                    self.close_iterator(host, iterator)?;
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
        let target_keys = unwrap_script!(self.own_keys(host, target, false));
        if !extensible && target_keys.len() != keys.len() {
            return Ok(Err(self.type_error(
                "Proxy ownKeys trap changed keys of a non-extensible target",
            )?));
        }
        for key in target_keys {
            let descriptor = unwrap_script!(self.own_descriptor(host, target, key.borrowed()));
            if (!extensible || descriptor.is_some_and(|(_, attrs)| !attrs.configurable))
                && !seen.contains(&key)
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
        id: ObjectId,
    ) -> Result<Result<Vec<String>, Value>, VmError> {
        let ordinary = !matches!(self.heap.get(id)?, Object::Proxy { .. });
        let keys = unwrap_script!(self.own_keys(host, id, ordinary));
        let mut names = Vec::new();
        for key in keys {
            if let PropertyKey::String(key) = key
                && (ordinary
                    || unwrap_script!(self.own_descriptor(host, id, PropertyKeyRef::String(&key)))
                        .is_some_and(|(_, attrs)| attrs.enumerable))
            {
                names.push(key);
            }
        }
        Ok(Ok(names))
    }

    pub(crate) fn own_descriptor(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Result<Option<(Property, Attributes)>, Value>, VmError> {
        let Object::Proxy {
            target, handler, ..
        } = *self.heap.get(id)?
        else {
            return Ok(Ok(self.heap.get(id)?.own_property(key).map(|property| {
                let attrs = property.attributes();
                (property.into_owned(), attrs)
            })));
        };
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            handler,
            "getOwnPropertyDescriptor",
            vec![Value::Object(target), key_value(key)]
        )) else {
            return self.own_descriptor(host, target, key);
        };
        if !matches!(result, Value::Undefined | Value::Object(_)) {
            return Ok(Err(self.type_error(
                "Proxy descriptor trap must return an object or undefined",
            )?));
        }
        let current = unwrap_script!(self.own_descriptor(host, target, key));
        let extensible = self
            .heap
            .get(self.heap.proxy_target(target)?)?
            .is_extensible();
        let invalid = |vm: &mut Self| {
            Ok(Err(vm.type_error(
                "Proxy descriptor trap reported an incompatible descriptor",
            )?))
        };
        if result == Value::Undefined {
            return if current.is_some_and(|(_, attrs)| !attrs.configurable || !extensible) {
                invalid(self)
            } else {
                Ok(Ok(None))
            };
        }
        let update = unwrap_script!(self.read_property_descriptor(host, &result));
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
        iterator: ObjectId,
    ) -> Result<Result<Option<String>, Value>, VmError> {
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
                return Ok(Ok(Some(index.to_string())));
            }
            let Some(id) = *current else {
                return Ok(Ok(None));
            };
            if let Some(keys) = pending {
                if let Some(key) = keys.pop_front() {
                    if visited.contains(&key)
                        || ops::canonical_index(&key)
                            .is_some_and(|index| index < string_indices.end)
                    {
                        continue;
                    }
                    let Some((_, attrs)) =
                        unwrap_script!(self.own_descriptor(host, id, PropertyKeyRef::String(&key)))
                    else {
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
                let keys = unwrap_script!(self.own_keys(host, id, false))
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
        PropertyKeyRef::String(key) => Value::String(key.to_owned()),
        PropertyKeyRef::Symbol(key) => Value::Symbol(key),
    }
}
