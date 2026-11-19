//! Ordered strong collections. Tombstones keep live cursor positions stable.

use crate::error::VmError;
use crate::heap::{
    Attributes, CollectionIteration, CollectionKind, DescriptorUpdate, ITERATOR_SYMBOL, Object,
    PlainObject,
};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

#[derive(Clone, Copy)]
pub(crate) enum CollectionMethod {
    Get,
    Insert,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    Iterate(CollectionIteration),
}

impl CollectionMethod {
    fn methods(kind: CollectionKind) -> impl Iterator<Item = (&'static str, Self)> {
        use CollectionIteration::{Entries, Keys, Values};
        use CollectionMethod::*;
        let specific: &[(_, _)] = match kind {
            CollectionKind::Map => &[("get", Get), ("set", Insert), ("keys", Iterate(Keys))],
            CollectionKind::Set => &[("add", Insert)],
        };
        specific.iter().copied().chain([
            ("has", Has),
            ("delete", Delete),
            ("clear", Clear),
            ("size", Size),
            ("forEach", ForEach),
            ("values", Iterate(Values)),
            ("entries", Iterate(Entries)),
        ])
    }

    pub(crate) fn lookup(kind: CollectionKind, name: &str) -> Option<Self> {
        Self::methods(kind).find_map(|(key, method)| (key == name).then_some(method))
    }
}

fn same_value_zero(a: &Value, b: &Value) -> bool {
    a == b || matches!((a, b), (Value::Number(a), Value::Number(b)) if a.is_nan() && b.is_nan())
}

fn canonical_key(value: &Value) -> Value {
    if matches!(value, Value::Number(x) if *x == 0.0) {
        Value::Number(0.0)
    } else {
        value.clone()
    }
}

impl Vm {
    pub(crate) fn init_collection(
        &mut self,
        kind: CollectionKind,
        identity: &Value,
    ) -> (ObjectId, ObjectId, ObjectId) {
        let (name, iterator_name) = match kind {
            CollectionKind::Map => ("Map", "MapIterator"),
            CollectionKind::Set => ("Set", "SetIterator"),
        };
        let constructor = self.heap.alloc_builtin(name, None);
        let prototype = self.heap.alloc_plain();
        let iterator = self.heap.alloc_plain();
        self.heap
            .define_data(
                constructor,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("collection constructor prototype");
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(constructor),
                Attributes::METHOD,
            )
            .expect("collection prototype constructor");
        for (name_key, method) in CollectionMethod::methods(kind) {
            let function = self.heap.alloc_builtin(name, Some(name_key));
            if matches!(method, CollectionMethod::Size) {
                assert!(
                    self.heap
                        .apply_descriptor(
                            prototype,
                            name_key.into(),
                            DescriptorUpdate {
                                get: Some(Some(function)),
                                configurable: Some(true),
                                ..DescriptorUpdate::default()
                            },
                        )
                        .expect("collection size accessor")
                );
                continue;
            }
            let value = Value::Object(function);
            self.heap
                .define_data(prototype, name_key, value.clone(), Attributes::METHOD)
                .expect("collection method");
            if kind == CollectionKind::Set && name_key == "values" {
                self.heap
                    .define_data(prototype, "keys", value.clone(), Attributes::METHOD)
                    .expect("Set keys alias");
            }
            if matches!(
                (kind, name_key),
                (CollectionKind::Set, "values") | (CollectionKind::Map, "entries")
            ) {
                self.heap
                    .define_data(
                        prototype,
                        PropertyKey::Symbol(ITERATOR_SYMBOL),
                        value,
                        Attributes::METHOD,
                    )
                    .expect("collection iterator alias");
            }
        }
        let next = self.heap.alloc_builtin(iterator_name, Some("next"));
        self.heap
            .define_data(iterator, "next", Value::Object(next), Attributes::METHOD)
            .expect("collection iterator next");
        self.heap
            .define_data(
                iterator,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                identity.clone(),
                Attributes::METHOD,
            )
            .expect("collection iterator identity");
        (constructor, prototype, iterator)
    }

    pub(crate) fn construct_collection(
        &mut self,
        host: &mut dyn Host,
        kind: CollectionKind,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let (default_proto, adder_name) = match kind {
            CollectionKind::Map => (self.builtins.map_prototype, "set"),
            CollectionKind::Set => (self.builtins.set_prototype, "add"),
        };
        let object = PlainObject::new(prototype.or(Some(default_proto)));
        let collection = Value::Object(self.heap.alloc(match kind {
            CollectionKind::Map => Object::Map {
                object,
                entries: Vec::new(),
            },
            CollectionKind::Set => Object::Set {
                object,
                elements: Vec::new(),
            },
        }));
        let Some(iterable) = args
            .first()
            .filter(|v| !matches!(v, Value::Null | Value::Undefined))
        else {
            return Ok(Invocation::Value(collection));
        };
        let adder =
            match self.read_property(host, &collection, PropertyKeyRef::String(adder_name))? {
                Invocation::Value(value) if self.is_callable(&value) => value,
                Invocation::Value(_) => {
                    return self.throw_type_error("collection adder is not callable");
                }
                thrown => return Ok(thrown),
            };
        let (iterator, next) = match self.get_iterator(host, iterable)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        loop {
            let item = match self.iterator_step(host, &iterator, &next)? {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(Invocation::Value(collection)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let outcome = self.collection_add_item(host, kind, &collection, &adder, item)?;
            if matches!(outcome, Invocation::Thrown(_)) {
                // IteratorClose preserves the original throw over a return-method throw.
                self.close_iterator(host, &iterator)?;
                return Ok(outcome);
            }
        }
    }

    fn collection_add_item(
        &mut self,
        host: &mut dyn Host,
        kind: CollectionKind,
        collection: &Value,
        adder: &Value,
        item: Value,
    ) -> Result<Invocation, VmError> {
        let args = if kind == CollectionKind::Map {
            if !matches!(item, Value::Object(_)) {
                return self.throw_type_error("Map entry must be an object");
            }
            let mut pair = Vec::with_capacity(2);
            for name in ["0", "1"] {
                match self.read_property(host, &item, PropertyKeyRef::String(name))? {
                    Invocation::Value(value) => pair.push(value),
                    thrown => return Ok(thrown),
                }
            }
            pair
        } else {
            vec![item]
        };
        self.invoke(host, adder, args, collection)
    }

    pub(crate) fn collection_method(
        &mut self,
        host: &mut dyn Host,
        kind: CollectionKind,
        method: CollectionMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        use CollectionMethod::*;
        let invalid = "incompatible collection receiver";
        let Value::Object(id) = *receiver else {
            return self.throw_type_error(invalid);
        };
        let object = self.heap.get_mut(id)?;
        if !matches!(
            (kind, &*object),
            (CollectionKind::Map, Object::Map { .. }) | (CollectionKind::Set, Object::Set { .. })
        ) {
            return self.throw_type_error(invalid);
        }
        let key = args.first().unwrap_or(&Value::Undefined);
        let second = args.get(1).unwrap_or(&Value::Undefined);
        let value = match method {
            Iterate(iteration) => {
                let prototype = match kind {
                    CollectionKind::Map => self.builtins.map_iterator_prototype,
                    CollectionKind::Set => self.builtins.set_iterator_prototype,
                };
                Value::Object(self.heap.alloc(Object::CollectionIterator {
                    object: PlainObject::new(Some(prototype)),
                    kind,
                    iteration,
                    target: Some(id),
                    next_index: 0,
                }))
            }
            ForEach => {
                if !self.is_callable(key) {
                    return self.throw_type_error("collection forEach callback is not callable");
                }
                let mut index = 0;
                while let Some((k, v)) = self.collection_pair(id, &mut index)? {
                    let result = self.invoke(host, key, vec![v, k, receiver.clone()], second)?;
                    if matches!(result, Invocation::Thrown(_)) {
                        return Ok(result);
                    }
                }
                Value::Undefined
            }
            _ => match object {
                Object::Map { entries, .. } => collection_slot_method(
                    entries,
                    method,
                    receiver,
                    |pair| same_value_zero(&pair.0, key),
                    || (canonical_key(key), second.clone()),
                    |pair| pair.1.clone(),
                    |pair| pair.1 = second.clone(),
                ),
                Object::Set { elements, .. } => collection_slot_method(
                    elements,
                    method,
                    receiver,
                    |value| same_value_zero(value, key),
                    || canonical_key(key),
                    |_| unreachable!("Set has no get"),
                    |_| {},
                ),
                _ => unreachable!("receiver was checked"),
            },
        };
        Ok(Invocation::Value(value))
    }

    fn collection_pair(
        &self,
        id: ObjectId,
        index: &mut usize,
    ) -> Result<Option<(Value, Value)>, VmError> {
        Ok(match self.heap.get(id)? {
            Object::Map { entries, .. } => next_slot(entries, index).cloned(),
            Object::Set { elements, .. } => {
                next_slot(elements, index).map(|value| (value.clone(), value.clone()))
            }
            _ => unreachable!("collection target was validated"),
        })
    }

    pub(crate) fn collection_next(
        &mut self,
        kind: CollectionKind,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid = "incompatible collection iterator receiver";
        let Value::Object(id) = *receiver else {
            return self.throw_type_error(invalid);
        };
        let Object::CollectionIterator {
            kind: actual,
            target,
            iteration,
            next_index,
            ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(invalid);
        };
        if *actual != kind {
            return self.throw_type_error(invalid);
        }
        let (iteration, mut index) = (*iteration, *next_index);
        let pair = match *target {
            Some(target) => self.collection_pair(target, &mut index)?,
            None => return self.iterator_result(Value::Undefined, true),
        };
        let Object::CollectionIterator {
            target, next_index, ..
        } = self.heap.get_mut(id)?
        else {
            unreachable!()
        };
        let Some((key, value)) = pair else {
            *target = None;
            *next_index = 0;
            return self.iterator_result(Value::Undefined, true);
        };
        *next_index = index;
        let value = match iteration {
            CollectionIteration::Keys => key,
            CollectionIteration::Values => value,
            CollectionIteration::Entries => Value::Object(self.heap.alloc_array(vec![key, value])),
        };
        self.iterator_result(value, false)
    }
}

fn next_slot<'a, T>(slots: &'a [Option<T>], index: &mut usize) -> Option<&'a T> {
    while let Some(slot) = slots.get(*index) {
        *index += 1;
        if let Some(value) = slot {
            return Some(value);
        }
    }
    None
}

fn collection_slot_method<T>(
    slots: &mut Vec<Option<T>>,
    method: CollectionMethod,
    receiver: &Value,
    matches: impl Fn(&T) -> bool,
    new_slot: impl FnOnce() -> T,
    get: impl FnOnce(&T) -> Value,
    update: impl FnOnce(&mut T),
) -> Value {
    use CollectionMethod::*;
    match method {
        Clear => {
            slots.fill_with(|| None);
            return Value::Undefined;
        }
        Size => return Value::Number(slots.iter().flatten().count() as f64),
        _ => {}
    }
    let slot = slots
        .iter_mut()
        .find(|slot| slot.as_ref().is_some_and(&matches));
    match method {
        Get => slot
            .and_then(|slot| slot.as_ref())
            .map_or(Value::Undefined, get),
        Has => Value::Bool(slot.is_some()),
        Delete => Value::Bool(slot.and_then(Option::take).is_some()),
        Insert => {
            if let Some(entry) = slot.and_then(|slot| slot.as_mut()) {
                update(entry);
            } else {
                slots.push(Some(new_slot()));
            }
            receiver.clone()
        }
        _ => unreachable!(),
    }
}
