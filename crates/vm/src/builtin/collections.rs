//! Strong and weak collections. Strong collection tombstones keep live cursor positions stable.

use crate::builtin::globals::Owner;
use crate::builtin::{RootKind, RootSpec, define_builtin_data, link_constructor};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, CollectionIteration, CollectionKind, ITERATOR_SYMBOL, Object, PlainObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
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

#[derive(Clone, Copy)]
enum WeakSetMethod {
    Add,
    Has,
    Delete,
}

const MAP: IntrinsicKey = IntrinsicKey::new("Map", Slot::Global, "");
const SET: IntrinsicKey = IntrinsicKey::new("Set", Slot::Global, "");
const WEAK_MAP: IntrinsicKey = IntrinsicKey::new("WeakMap", Slot::Global, "");
const WEAK_SET: IntrinsicKey = IntrinsicKey::new("WeakSet", Slot::Global, "");
const INVALID_WEAK_KEY: &str = "weak collection key must be an object or non-registered symbol";

pub(crate) const MAP_OWNER: Owner = Owner::new(
    map_members::ROOT,
    MAP_ROOTS,
    &[MAP_INTRINSICS, map_members::INTRINSICS],
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
    set_entries::ROOT,
    SET_ROOTS,
    &[
        SET_INTRINSICS,
        set_members::INTRINSICS,
        set_entries::INTRINSICS,
    ],
    Some(|vm, identity| {
        vm.install_collection(
            CollectionKind::Set,
            vm.builtins.get(set_members::ROOT),
            vm.builtins.get("set.prototype"),
            vm.builtins.get("setIterator.prototype"),
            identity,
        );
    }),
);
pub(crate) const WEAK_MAP_OWNER: Owner = Owner::new(
    weak_map_members::ROOT,
    WEAK_MAP_ROOTS,
    &[WEAK_MAP_INTRINSICS, weak_map_members::INTRINSICS],
    Some(|vm, _| {
        let constructor = vm.builtins.get("weakmap");
        let prototype = vm.builtins.get("weakmap.prototype");
        link_constructor(vm, constructor, prototype);
        weak_map_members::install_members(vm, constructor, prototype);
    }),
);
pub(crate) const WEAK_SET_OWNER: Owner = Owner::new(
    weak_set_members::ROOT,
    WEAK_SET_ROOTS,
    &[WEAK_SET_INTRINSICS, weak_set_members::INTRINSICS],
    Some(|vm, _| {
        let constructor = vm.builtins.get("weakset");
        let prototype = vm.builtins.get("weakset.prototype");
        link_constructor(vm, constructor, prototype);
        weak_set_members::install_members(vm, constructor, prototype);
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
const WEAK_SET_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "weakset",
        RootKind::Native(WEAK_SET),
        Some("function.prototype"),
    )
    .fixed("weakset.prototype"),
    RootSpec::new(
        "weakset.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
];

#[yield_binding_macros::builtin_global(name = "Map", root = "map", kind = "constructor")]
mod map_members {
    use super::{CollectionIteration::*, CollectionKind, CollectionMethod::*};

    prototype_calls! {
        get => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Get, receiver, &args)
        };
        set => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Insert, receiver, &args)
        };
        keys => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Keys), receiver, &args)
        };
        has => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Has, receiver, &args)
        };
        delete => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Delete, receiver, &args)
        };
        clear => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Clear, receiver, &args)
        };
        get size => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Size, receiver, &args)
        };
        forEach => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, ForEach, receiver, &args)
        };
        values => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Values), receiver, &args)
        };
        entries => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Entries), receiver, &args)
        };
    }
}

#[yield_binding_macros::builtin_global(name = "Set", root = "set", kind = "constructor")]
mod set_members {
    use super::{CollectionIteration::Values, CollectionKind, CollectionMethod::*};

    prototype_calls! {
        add => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Insert, receiver, &args)
        };
        has => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Has, receiver, &args)
        };
        delete => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Delete, receiver, &args)
        };
        clear => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Clear, receiver, &args)
        };
        get size => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Size, receiver, &args)
        };
        forEach => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, ForEach, receiver, &args)
        };
        values => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Iterate(Values), receiver, &args)
        };
    }
}

// Set's aliases are installed between `values` and `entries`.
#[yield_binding_macros::builtin_global(name = "Set", root = "set", kind = "constructor")]
mod set_entries {
    use super::{CollectionIteration::Entries, CollectionKind, CollectionMethod::Iterate};

    prototype_calls! {
        entries => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Iterate(Entries), receiver, &args)
        };
    }
}

const MAP_ITERATOR_NEXT: IntrinsicKey = IntrinsicKey::new("MapIterator", Slot::Prototype, "next");
const SET_ITERATOR_NEXT: IntrinsicKey = IntrinsicKey::new("SetIterator", Slot::Prototype, "next");

const MAP_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        MAP,
        |vm, _, _, _, _| vm.throw_type_error("Map and Set require new"),
        |vm, host, roots, args, prototype| {
            vm.construct_collection(host, roots, CollectionKind::Map, &args, prototype)
        },
    ),
    IntrinsicDef::call(MAP_ITERATOR_NEXT, |vm, _, _, _, receiver| {
        vm.collection_next(CollectionKind::Map, receiver)
    }),
];

const SET_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        SET,
        |vm, _, _, _, _| vm.throw_type_error("Map and Set require new"),
        |vm, host, roots, args, prototype| {
            vm.construct_collection(host, roots, CollectionKind::Set, &args, prototype)
        },
    ),
    IntrinsicDef::call(SET_ITERATOR_NEXT, |vm, _, _, _, receiver| {
        vm.collection_next(CollectionKind::Set, receiver)
    }),
];

#[yield_binding_macros::builtin_global(name = "WeakMap", root = "weakmap", kind = "constructor")]
mod weak_map_members {
    use super::WeakMapMethod::*;

    prototype_calls! {
        get => |vm, _, _, args, receiver| vm.weakmap_method(Get, receiver, &args);
        set => |vm, _, _, args, receiver| vm.weakmap_method(Set, receiver, &args);
        has => |vm, _, _, args, receiver| vm.weakmap_method(Has, receiver, &args);
        delete => |vm, _, _, args, receiver| vm.weakmap_method(Delete, receiver, &args);
    }
}

const WEAK_MAP_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::constructor(
    WEAK_MAP,
    |vm, _, _, _, _| vm.throw_type_error("WeakMap requires new"),
    |vm, host, roots, args, prototype| vm.construct_weakmap(host, roots, &args, prototype),
)];

#[yield_binding_macros::builtin_global(name = "WeakSet", root = "weakset", kind = "constructor")]
mod weak_set_members {
    use super::WeakSetMethod::*;

    prototype_calls! {
        add => |vm, _, _, args, receiver| vm.weakset_method(Add, receiver, &args);
        has => |vm, _, _, args, receiver| vm.weakset_method(Has, receiver, &args);
        delete => |vm, _, _, args, receiver| vm.weakset_method(Delete, receiver, &args);
    }
}

const WEAK_SET_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::constructor(
    WEAK_SET,
    |vm, _, _, _, _| vm.throw_type_error("WeakSet requires new"),
    |vm, host, roots, args, prototype| vm.construct_weakset(host, roots, &args, prototype),
)];

fn same_value_zero(a: &Value, b: &Value) -> bool {
    a == b
        || matches!((a.view(), b.view()), (ValueRef::Number(a), ValueRef::Number(b)) if a.is_nan() && b.is_nan())
}

fn canonical_key(value: &Value) -> Value {
    if matches!(value.view(), ValueRef::Number(x) if x == 0.0) {
        Value::number(0.0)
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
        link_constructor(self, constructor, prototype);
        let iterator_key = PropertyKey::Symbol(ITERATOR_SYMBOL);
        let next = match kind {
            CollectionKind::Map => {
                map_members::install_members(self, constructor, prototype);
                let entries = Value::object(self.collection_alias_source(prototype, "entries"));
                define_builtin_data(self, prototype, iterator_key, entries, Attributes::METHOD);
                MAP_ITERATOR_NEXT
            }
            CollectionKind::Set => {
                set_members::install_members(self, constructor, prototype);
                let values = self.collection_alias_source(prototype, "values");
                define_builtin_data(
                    self,
                    prototype,
                    "keys",
                    Value::object(values),
                    Attributes::METHOD,
                );
                define_builtin_data(
                    self,
                    prototype,
                    iterator_key,
                    Value::object(values),
                    Attributes::METHOD,
                );
                set_entries::install_members(self, constructor, prototype);
                SET_ITERATOR_NEXT
            }
        };
        let next = self.alloc_intrinsic(next);
        self.heap
            .define_data(iterator, "next", Value::object(next), Attributes::METHOD)
            .expect("collection iterator next");
        self.heap
            .define_data(
                iterator,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                identity.clone(),
                Attributes::METHOD,
            )
            .expect("collection iterator identity");
    }

    fn collection_alias_source(&self, prototype: ObjectId, name: &str) -> ObjectId {
        let property = self
            .heap
            .get(prototype)
            .expect("collection prototype")
            .own_property(PropertyKeyRef::String(name))
            .expect("collection alias source");
        let Some(ValueRef::Object(id)) = property.data_value().map(Value::view) else {
            unreachable!("collection alias source is an own data property holding a function")
        };
        id
    }

    fn construct_weakmap(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get("weakmap.prototype"));
        let object = PlainObject::new(Some(prototype));
        let collection = Value::object(self.heap.alloc(Object::WeakMap {
            object,
            entries: Default::default(),
        }));
        self.populate_collection(host, roots, collection, args, "set")
    }

    fn weakmap_method(
        &mut self,
        method: WeakMapMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error("incompatible WeakMap receiver");
        };
        if !matches!(self.heap.get(id)?, Object::WeakMap { .. }) {
            return self.throw_type_error("incompatible WeakMap receiver");
        }
        let Some(key) = args.first().and_then(|value| self.heap.weak_key(value)) else {
            return match method {
                WeakMapMethod::Set => self.throw_type_error(INVALID_WEAK_KEY),
                WeakMapMethod::Get => Ok(Invocation::Value(Value::UNDEFINED)),
                WeakMapMethod::Has | WeakMapMethod::Delete => {
                    Ok(Invocation::Value(Value::boolean(false)))
                }
            };
        };
        let Object::WeakMap { entries, .. } = self.heap.get_mut(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(Invocation::Value(match method {
            WeakMapMethod::Get => entries.get(&key).cloned().unwrap_or(Value::UNDEFINED),
            WeakMapMethod::Set => {
                entries.insert(key, args.get(1).cloned().unwrap_or(Value::UNDEFINED));
                receiver.clone()
            }
            WeakMapMethod::Has => Value::boolean(entries.contains_key(&key)),
            WeakMapMethod::Delete => Value::boolean(entries.remove(&key).is_some()),
        }))
    }

    fn construct_weakset(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get("weakset.prototype"));
        let collection = Value::object(self.heap.alloc(Object::WeakSet {
            object: PlainObject::new(Some(prototype)),
            entries: Default::default(),
        }));
        self.populate_collection(host, roots, collection, args, "add")
    }

    fn weakset_method(
        &mut self,
        method: WeakSetMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error("incompatible WeakSet receiver");
        };
        if !matches!(self.heap.get(id)?, Object::WeakSet { .. }) {
            return self.throw_type_error("incompatible WeakSet receiver");
        }
        let Some(key) = args.first().and_then(|value| self.heap.weak_key(value)) else {
            return match method {
                WeakSetMethod::Add => self.throw_type_error(INVALID_WEAK_KEY),
                WeakSetMethod::Has | WeakSetMethod::Delete => {
                    Ok(Invocation::Value(Value::boolean(false)))
                }
            };
        };
        let Object::WeakSet { entries, .. } = self.heap.get_mut(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(Invocation::Value(match method {
            WeakSetMethod::Add => {
                entries.insert(key);
                receiver.clone()
            }
            WeakSetMethod::Has => Value::boolean(entries.contains(&key)),
            WeakSetMethod::Delete => Value::boolean(entries.remove(&key)),
        }))
    }

    fn construct_collection(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        kind: CollectionKind,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let (default_proto, adder_name) = match kind {
            CollectionKind::Map => (self.builtins.get("map.prototype"), "set"),
            CollectionKind::Set => (self.builtins.get("set.prototype"), "add"),
        };
        let object = PlainObject::new(prototype.or(Some(default_proto)));
        let collection = Value::object(self.heap.alloc(match kind {
            CollectionKind::Map => Object::Map {
                object,
                entries: Vec::new(),
            },
            CollectionKind::Set => Object::Set {
                object,
                elements: Vec::new(),
            },
        }));
        self.populate_collection(host, roots, collection, args, adder_name)
    }

    fn populate_collection(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        collection: Value,
        args: &[Value],
        adder_name: &str,
    ) -> Result<Invocation, VmError> {
        let root_view = (&collection, args);
        let roots = &roots.with(&root_view);
        let Some(iterable) = args
            .first()
            .filter(|v| !matches!(v.view(), ValueRef::Null | ValueRef::Undefined))
        else {
            return Ok(Invocation::Value(collection));
        };
        let adder = match self.read_property(
            host,
            roots,
            &collection,
            PropertyKeyRef::String(adder_name),
        )? {
            Invocation::Value(value) if self.is_callable(&value) => value,
            Invocation::Value(_) => {
                return self.throw_type_error("collection adder is not callable");
            }
            thrown => return Ok(thrown),
        };
        let roots = &roots.with(&adder);
        let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        loop {
            let root_view = (&iterator, &next);
            let roots = &roots.with(&root_view);
            let item = match self.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(Invocation::Value(collection)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let outcome = self.collection_add_item(
                host,
                roots,
                adder_name == "set",
                &collection,
                &adder,
                item,
            )?;
            if let Invocation::Thrown(error) = &outcome {
                // IteratorClose preserves the original throw over a return-method throw.
                self.close_iterator(host, &roots.with(&error), &iterator)?;
                return Ok(outcome);
            }
        }
    }

    fn collection_add_item(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        entry_pairs: bool,
        collection: &Value,
        adder: &Value,
        item: Value,
    ) -> Result<Invocation, VmError> {
        let args = if entry_pairs {
            if !matches!(item.view(), ValueRef::Object(_)) {
                return self.throw_type_error("collection entry must be an object");
            }
            let mut pair = Vec::with_capacity(2);
            for name in ["0", "1"] {
                let root_view = (&item, &pair);
                let roots = &roots.with(&root_view);
                match self.read_property(host, roots, &item, PropertyKeyRef::String(name))? {
                    Invocation::Value(value) => pair.push(value),
                    thrown => return Ok(thrown),
                }
            }
            pair
        } else {
            vec![item]
        };
        self.invoke(host, roots, adder, args, collection)
    }

    fn collection_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        kind: CollectionKind,
        method: CollectionMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        use CollectionMethod::*;
        let invalid = "incompatible collection receiver";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(invalid);
        };
        let object = self.heap.get_mut(id)?;
        if !matches!(
            (kind, &*object),
            (CollectionKind::Map, Object::Map { .. }) | (CollectionKind::Set, Object::Set { .. })
        ) {
            return self.throw_type_error(invalid);
        }
        let undefined = Value::UNDEFINED;
        let key = args.first().unwrap_or(&undefined);
        let second = args.get(1).unwrap_or(&undefined);
        let value = match method {
            Iterate(iteration) => {
                let prototype = match kind {
                    CollectionKind::Map => self.builtins.get("mapIterator.prototype"),
                    CollectionKind::Set => self.builtins.get("setIterator.prototype"),
                };
                Value::object(self.heap.alloc(Object::CollectionIterator {
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
                    let result =
                        self.invoke(host, roots, key, vec![v, k, receiver.clone()], second)?;
                    if matches!(result, Invocation::Thrown(_)) {
                        return Ok(result);
                    }
                }
                Value::UNDEFINED
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
        let ValueRef::Object(id) = receiver.view() else {
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
            None => return self.iterator_result(Value::UNDEFINED, true),
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
            return self.iterator_result(Value::UNDEFINED, true);
        };
        *next_index = index;
        let value = match iteration {
            CollectionIteration::Keys => key,
            CollectionIteration::Values => value,
            CollectionIteration::Entries => Value::object(self.heap.alloc_array(vec![key, value])),
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
            return Value::UNDEFINED;
        }
        Size => return Value::number(slots.iter().flatten().count() as f64),
        _ => {}
    }
    let slot = slots
        .iter_mut()
        .find(|slot| slot.as_ref().is_some_and(&matches));
    match method {
        Get => slot
            .and_then(|slot| slot.as_ref())
            .map_or(Value::UNDEFINED, get),
        Has => Value::boolean(slot.is_some()),
        Delete => Value::boolean(slot.and_then(Option::take).is_some()),
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
