//! Strong and weak collections. Strong collection tombstones keep live cursor positions stable.

use crate::builtin_globals::Owner;
use crate::builtins::{RootKind, RootSpec};
use crate::error::VmError;
use crate::heap::{
    Attributes, CollectionIteration, CollectionKind, DescriptorUpdate, ITERATOR_SYMBOL, Object,
    PlainObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

#[derive(Clone, Copy)]
enum CollectionMethod {
    Get,
    Insert,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    Iterate(CollectionIteration),
}

#[derive(Clone, Copy)]
enum WeakMapMethod {
    Get,
    Set,
    Has,
    Delete,
}

const MAP: IntrinsicKey = IntrinsicKey::new("Map", Slot::Global, "");
const SET: IntrinsicKey = IntrinsicKey::new("Set", Slot::Global, "");
const WEAK_MAP: IntrinsicKey = IntrinsicKey::new("WeakMap", Slot::Global, "");

pub(crate) const MAP_OWNER: Owner = Owner::new(
    "map",
    MAP_ROOTS,
    &[MAP_INTRINSICS],
    Some(|vm, identity| {
        vm.install_collection(
            CollectionKind::Map,
            vm.builtins.get("map"),
            vm.builtins.get("map.prototype"),
            vm.builtins.get("mapIterator.prototype"),
            identity,
        );
    }),
);
pub(crate) const SET_OWNER: Owner = Owner::new(
    "set",
    SET_ROOTS,
    &[SET_INTRINSICS],
    Some(|vm, identity| {
        vm.install_collection(
            CollectionKind::Set,
            vm.builtins.get("set"),
            vm.builtins.get("set.prototype"),
            vm.builtins.get("setIterator.prototype"),
            identity,
        );
    }),
);
pub(crate) const WEAK_MAP_OWNER: Owner = Owner::new(
    "weakmap",
    WEAK_MAP_ROOTS,
    &[WEAK_MAP_INTRINSICS],
    Some(|vm, _| {
        vm.install_weakmap(
            vm.builtins.get("weakmap"),
            vm.builtins.get("weakmap.prototype"),
        );
    }),
);

pub(crate) const MAP_ROOTS: &[RootSpec] = &[
    RootSpec::new("map", RootKind::Native(MAP), Some("function.prototype")).fixed("map.prototype"),
    RootSpec::new("map.prototype", RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        "mapIterator.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
];
pub(crate) const SET_ROOTS: &[RootSpec] = &[
    RootSpec::new("set", RootKind::Native(SET), Some("function.prototype")).fixed("set.prototype"),
    RootSpec::new("set.prototype", RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        "setIterator.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
];
pub(crate) const WEAK_MAP_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "weakmap",
        RootKind::Native(WEAK_MAP),
        Some("function.prototype"),
    )
    .fixed("weakmap.prototype"),
    RootSpec::new(
        "weakmap.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
];

use CollectionIteration::{Entries, Keys, Values};
use CollectionMethod::*;

macro_rules! collection {
    ($key:expr, $owner:literal, $iterator:literal, $kind:ident; $($name:literal => $method:expr),*) => {
        &[
            IntrinsicDef::constructor(
                $key,
                |vm, _, _, _| vm.throw_type_error("Map and Set require new"),
                |vm, host, args, prototype| vm.construct_collection(host, CollectionKind::$kind, &args, prototype),
            ),
            $(collection!(@method $owner, $kind, Prototype, $name, $method),)*
            collection!(@method $owner, $kind, Prototype, "has", Has),
            collection!(@method $owner, $kind, Prototype, "delete", Delete),
            collection!(@method $owner, $kind, Prototype, "clear", Clear),
            collection!(@method $owner, $kind, PrototypeGetter, "size", Size),
            collection!(@method $owner, $kind, Prototype, "forEach", ForEach),
            collection!(@method $owner, $kind, Prototype, "values", Iterate(Values)),
            collection!(@method $owner, $kind, Prototype, "entries", Iterate(Entries)),
            IntrinsicDef::call(IntrinsicKey::new($iterator, Slot::Prototype, "next"), |vm, _, _, receiver| {
                vm.collection_next(CollectionKind::$kind, receiver)
            }),
        ]
    };
    (@method $owner:literal, $kind:ident, $slot:ident, $name:literal, $method:expr) => {
        IntrinsicDef::call(IntrinsicKey::new($owner, Slot::$slot, $name), |vm, host, args, receiver| {
            vm.collection_method(host, CollectionKind::$kind, $method, receiver, &args)
        })
    };
}

pub(crate) const MAP_INTRINSICS: &[IntrinsicDef] = collection!(
    MAP, "Map", "MapIterator", Map; "get" => Get, "set" => Insert, "keys" => Iterate(Keys)
);
pub(crate) const SET_INTRINSICS: &[IntrinsicDef] = collection!(
    SET, "Set", "SetIterator", Set; "add" => Insert
);

macro_rules! weak_map_method {
    ($name:literal, $method:ident) => {
        IntrinsicDef::call(
            IntrinsicKey::new("WeakMap", Slot::Prototype, $name),
            |vm, _, args, receiver| vm.weakmap_method(WeakMapMethod::$method, receiver, &args),
        )
    };
}

pub(crate) const WEAK_MAP_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        WEAK_MAP,
        |vm, _, _, _| vm.throw_type_error("WeakMap requires new"),
        |vm, host, args, prototype| vm.construct_weakmap(host, &args, prototype),
    ),
    weak_map_method!("get", Get),
    weak_map_method!("set", Set),
    weak_map_method!("has", Has),
    weak_map_method!("delete", Delete),
];

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
    pub(crate) fn install_collection(
        &mut self,
        kind: CollectionKind,
        constructor: ObjectId,
        prototype: ObjectId,
        iterator: ObjectId,
        identity: &Value,
    ) {
        self.link_collection_prototype(constructor, prototype);
        let defs = match kind {
            CollectionKind::Map => MAP_INTRINSICS,
            CollectionKind::Set => SET_INTRINSICS,
        };
        for def in defs {
            if def.key.slot == Slot::Global {
                continue;
            }
            let function = self.alloc_intrinsic(def.key);
            let name_key = def.key.member;
            if def.key.slot == Slot::PrototypeGetter {
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
            let target = if matches!(def.key.owner, "MapIterator" | "SetIterator") {
                iterator
            } else {
                prototype
            };
            self.heap
                .define_data(
                    target,
                    name_key,
                    Value::Object(function),
                    Attributes::METHOD,
                )
                .expect("collection method");
            // The aliases reuse this declaration's function object.
            if kind == CollectionKind::Set && name_key == "values" {
                self.heap
                    .define_data(
                        prototype,
                        "keys",
                        Value::Object(function),
                        Attributes::METHOD,
                    )
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
                        Value::Object(function),
                        Attributes::METHOD,
                    )
                    .expect("collection iterator alias");
            }
        }
        self.heap
            .define_data(
                iterator,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                identity.clone(),
                Attributes::METHOD,
            )
            .expect("collection iterator identity");
    }

    fn link_collection_prototype(&mut self, constructor: ObjectId, prototype: ObjectId) {
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
    }

    pub(crate) fn install_weakmap(&mut self, constructor: ObjectId, prototype: ObjectId) {
        self.link_collection_prototype(constructor, prototype);
        for def in WEAK_MAP_INTRINSICS {
            if def.key.slot == Slot::Global {
                continue;
            }
            let function = self.alloc_intrinsic(def.key);
            self.heap
                .define_data(
                    prototype,
                    def.key.member,
                    Value::Object(function),
                    Attributes::METHOD,
                )
                .expect("WeakMap method");
        }
    }

    fn construct_weakmap(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get("weakmap.prototype"));
        let object = PlainObject::new(Some(prototype));
        let collection = Value::Object(self.heap.alloc(Object::WeakMap {
            object,
            entries: Default::default(),
        }));
        self.populate_collection(host, collection, args, "set")
    }

    fn weakmap_method(
        &mut self,
        method: WeakMapMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = *receiver else {
            return self.throw_type_error("incompatible WeakMap receiver");
        };
        let Object::WeakMap { entries, .. } = self.heap.get_mut(id)? else {
            return self.throw_type_error("incompatible WeakMap receiver");
        };
        let Some(&Value::Object(key)) = args.first() else {
            return self.throw_type_error("WeakMap key must be an object");
        };
        Ok(Invocation::Value(match method {
            WeakMapMethod::Get => entries.get(&key).cloned().unwrap_or(Value::Undefined),
            WeakMapMethod::Set => {
                entries.insert(key, args.get(1).cloned().unwrap_or(Value::Undefined));
                receiver.clone()
            }
            WeakMapMethod::Has => Value::Bool(entries.contains_key(&key)),
            WeakMapMethod::Delete => Value::Bool(entries.remove(&key).is_some()),
        }))
    }

    fn construct_collection(
        &mut self,
        host: &mut dyn Host,
        kind: CollectionKind,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let (default_proto, adder_name) = match kind {
            CollectionKind::Map => (self.builtins.get("map.prototype"), "set"),
            CollectionKind::Set => (self.builtins.get("set.prototype"), "add"),
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
        self.populate_collection(host, collection, args, adder_name)
    }

    fn populate_collection(
        &mut self,
        host: &mut dyn Host,
        collection: Value,
        args: &[Value],
        adder_name: &str,
    ) -> Result<Invocation, VmError> {
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
            let outcome =
                self.collection_add_item(host, adder_name == "set", &collection, &adder, item)?;
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
        entry_pairs: bool,
        collection: &Value,
        adder: &Value,
        item: Value,
    ) -> Result<Invocation, VmError> {
        let args = if entry_pairs {
            if !matches!(item, Value::Object(_)) {
                return self.throw_type_error("collection entry must be an object");
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

    fn collection_method(
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
                    CollectionKind::Map => self.builtins.get("mapIterator.prototype"),
                    CollectionKind::Set => self.builtins.get("setIterator.prototype"),
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

    fn collection_next(
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
