//! Strong and weak collections. Strong collection tombstones keep live cursor positions stable.

use crate::builtin::globals::Owner;
use crate::builtin::{RootKind, RootSpec, define_builtin_data, link_constructor};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, CollectionIteration, CollectionKind, ITERATOR_SYMBOL, Object, PlainObject,
    TO_STRING_TAG_SYMBOL,
};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Clone, Copy)]
enum CollectionMethod {
    Get,
    Insert,
    GetOrInsert,
    GetOrInsertComputed,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    Iterate(CollectionIteration),
}

#[derive(Clone, Copy)]
enum SetOperation {
    Union,
    Intersection,
    Difference,
    SymmetricDifference,
    IsSubsetOf,
    IsSupersetOf,
    IsDisjointFrom,
}

/// GetSetRecord result: a set-like object with its size and cached methods.
struct SetRecord {
    object: Value,
    size: f64,
    has: Value,
    keys: Value,
}

#[derive(Clone, Copy)]
enum WeakMapMethod {
    Get,
    Set,
    GetOrInsert,
    GetOrInsertComputed,
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

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const MAP_OWNER: Owner<P> = Owner::new(
        map_members::ROOT,
        MAP_ROOTS,
        &[Self::MAP_INTRINSICS, map_members::Globals::<P>::INTRINSICS],
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
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const SET_OWNER: Owner<P> = Owner::new(
        set_entries::ROOT,
        SET_ROOTS,
        &[
            Self::SET_INTRINSICS,
            set_members::Globals::<P>::INTRINSICS,
            set_entries::Globals::<P>::INTRINSICS,
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
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const WEAK_MAP_OWNER: Owner<P> = Owner::new(
        weak_map_members::ROOT,
        WEAK_MAP_ROOTS,
        &[
            Self::WEAK_MAP_INTRINSICS,
            weak_map_members::Globals::<P>::INTRINSICS,
        ],
        Some(|vm, _| {
            let constructor = vm.builtins.get("weakmap");
            let prototype = vm.builtins.get("weakmap.prototype");
            link_constructor(vm, constructor, prototype);
            weak_map_members::install_members(vm, constructor, prototype);
        }),
    );
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const WEAK_SET_OWNER: Owner<P> = Owner::new(
        weak_set_members::ROOT,
        WEAK_SET_ROOTS,
        &[
            Self::WEAK_SET_INTRINSICS,
            weak_set_members::Globals::<P>::INTRINSICS,
        ],
        Some(|vm, _| {
            let constructor = vm.builtins.get("weakset");
            let prototype = vm.builtins.get("weakset.prototype");
            link_constructor(vm, constructor, prototype);
            weak_set_members::install_members(vm, constructor, prototype);
        }),
    );
}

pub(crate) const MAP_ROOTS: &[RootSpec] = &[
    RootSpec::new("map", RootKind::Native(MAP), Some("function.prototype")).fixed("map.prototype"),
    RootSpec::new("map.prototype", RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        "mapIterator.prototype",
        RootKind::Plain,
        Some(super::iterator::SYNC_ROOT),
    ),
];
pub(crate) const SET_ROOTS: &[RootSpec] = &[
    RootSpec::new("set", RootKind::Native(SET), Some("function.prototype")).fixed("set.prototype"),
    RootSpec::new("set.prototype", RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        "setIterator.prototype",
        RootKind::Plain,
        Some(super::iterator::SYNC_ROOT),
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

    static_calls! {
        groupBy: 2 => |vm, host, roots, args, _| vm.map_group_by(host, roots, &args);
    }

    prototype_calls! {
        get: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Get, receiver, &args)
        };
        getOrInsert: 2 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, GetOrInsert, receiver, &args)
        };
        getOrInsertComputed: 2 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, GetOrInsertComputed, receiver, &args)
        };
        set: 2 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Insert, receiver, &args)
        };
        keys: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Keys), receiver, &args)
        };
        has: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Has, receiver, &args)
        };
        delete: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Delete, receiver, &args)
        };
        clear: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Clear, receiver, &args)
        };
        get size: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Size, receiver, &args)
        };
        forEach: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, ForEach, receiver, &args)
        };
        values: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Values), receiver, &args)
        };
        entries: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Map, Iterate(Entries), receiver, &args)
        };
    }
}

#[yield_binding_macros::builtin_global(name = "Set", root = "set", kind = "constructor")]
mod set_members {
    use super::{CollectionIteration::Values, CollectionKind, CollectionMethod::*};

    prototype_calls! {
        add: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Insert, receiver, &args)
        };
        has: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Has, receiver, &args)
        };
        delete: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Delete, receiver, &args)
        };
        clear: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Clear, receiver, &args)
        };
        get size: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Size, receiver, &args)
        };
        forEach: 1 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, ForEach, receiver, &args)
        };
        values: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Iterate(Values), receiver, &args)
        };
    }
}

// Set's aliases are installed between `values` and `entries`.
#[yield_binding_macros::builtin_global(name = "Set", root = "set", kind = "constructor")]
mod set_entries {
    use super::{
        CollectionIteration::Entries, CollectionKind, CollectionMethod::Iterate, SetOperation,
    };

    prototype_calls! {
        entries: 0 => |vm, host, roots, args, receiver| {
            vm.collection_method(host, roots, CollectionKind::Set, Iterate(Entries), receiver, &args)
        };
        union: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::Union, receiver, &args)
        };
        intersection: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::Intersection, receiver, &args)
        };
        difference: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::Difference, receiver, &args)
        };
        symmetricDifference: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::SymmetricDifference, receiver, &args)
        };
        isSubsetOf: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::IsSubsetOf, receiver, &args)
        };
        isSupersetOf: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::IsSupersetOf, receiver, &args)
        };
        isDisjointFrom: 1 => |vm, host, roots, args, receiver| {
            vm.set_operation(host, roots, SetOperation::IsDisjointFrom, receiver, &args)
        };
    }
}

const MAP_ITERATOR_NEXT: IntrinsicKey = IntrinsicKey::new("MapIterator", Slot::Prototype, "next");
const SET_ITERATOR_NEXT: IntrinsicKey = IntrinsicKey::new("SetIterator", Slot::Prototype, "next");

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const MAP_INTRINSICS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::constructor(
            MAP,
            1,
            |vm, _, roots, _, _| vm.throw_type_error(roots, "Map and Set require new"),
            |vm, host, roots, args, prototype| {
                vm.construct_collection(host, roots, CollectionKind::Map, &args, prototype)
            },
        ),
        IntrinsicDef::call(MAP_ITERATOR_NEXT, 0, |vm, _, roots, _, receiver| {
            vm.collection_next(roots, CollectionKind::Map, receiver)
        }),
    ];
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const SET_INTRINSICS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::constructor(
            SET,
            1,
            |vm, _, roots, _, _| vm.throw_type_error(roots, "Map and Set require new"),
            |vm, host, roots, args, prototype| {
                vm.construct_collection(host, roots, CollectionKind::Set, &args, prototype)
            },
        ),
        IntrinsicDef::call(SET_ITERATOR_NEXT, 0, |vm, _, roots, _, receiver| {
            vm.collection_next(roots, CollectionKind::Set, receiver)
        }),
    ];
}

#[yield_binding_macros::builtin_global(name = "WeakMap", root = "weakmap", kind = "constructor")]
mod weak_map_members {
    use super::WeakMapMethod::*;

    prototype_calls! {
        get: 1 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, Get, receiver, &args);
        set: 2 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, Set, receiver, &args);
        has: 1 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, Has, receiver, &args);
        delete: 1 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, Delete, receiver, &args);
        getOrInsert: 2 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, GetOrInsert, receiver, &args);
        getOrInsertComputed: 2 => |vm, host, roots, args, receiver| vm.weakmap_method(host, roots, GetOrInsertComputed, receiver, &args);
    }
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const WEAK_MAP_INTRINSICS: &[IntrinsicDef<P>] = &[IntrinsicDef::constructor(
        WEAK_MAP,
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "WeakMap requires new"),
        |vm, host, roots, args, prototype| vm.construct_weakmap(host, roots, &args, prototype),
    )];
}

#[yield_binding_macros::builtin_global(name = "WeakSet", root = "weakset", kind = "constructor")]
mod weak_set_members {
    use super::WeakSetMethod::*;

    prototype_calls! {
        add: 1 => |vm, _, roots, args, receiver| vm.weakset_method(roots, Add, receiver, &args);
        has: 1 => |vm, _, roots, args, receiver| vm.weakset_method(roots, Has, receiver, &args);
        delete: 1 => |vm, _, roots, args, receiver| vm.weakset_method(roots, Delete, receiver, &args);
    }
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const WEAK_SET_INTRINSICS: &[IntrinsicDef<P>] = &[IntrinsicDef::constructor(
        WEAK_SET,
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "WeakSet requires new"),
        |vm, host, roots, args, prototype| vm.construct_weakset(host, roots, &args, prototype),
    )];
}

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

impl VmCore {
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
        let tag = match kind {
            CollectionKind::Map => "Map",
            CollectionKind::Set => "Set",
        };
        define_builtin_data(
            self,
            prototype,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(tag.into()),
            Attributes::FUNCTION_METADATA,
        );
        define_builtin_data(
            self,
            iterator,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(format!("{tag} Iterator").into()),
            Attributes::FUNCTION_METADATA,
        );
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

    fn construct_weakmap<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn weakmap_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: WeakMapMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "incompatible WeakMap receiver");
        };
        if !matches!(self.heap.get(id)?, Object::WeakMap { .. }) {
            return self.throw_type_error(roots, "incompatible WeakMap receiver");
        }
        let Some(key) = args.first().and_then(|value| self.heap.weak_key(value)) else {
            return match method {
                WeakMapMethod::Set
                | WeakMapMethod::GetOrInsert
                | WeakMapMethod::GetOrInsertComputed => {
                    self.throw_type_error(roots, INVALID_WEAK_KEY)
                }
                WeakMapMethod::Get => Ok(Invocation::Value(Value::UNDEFINED)),
                WeakMapMethod::Has | WeakMapMethod::Delete => {
                    Ok(Invocation::Value(Value::boolean(false)))
                }
            };
        };
        let undefined = Value::UNDEFINED;
        let second = args.get(1).unwrap_or(&undefined);
        if matches!(method, WeakMapMethod::GetOrInsertComputed) {
            if !self.is_callable(second) {
                return self.throw_type_error(
                    roots,
                    "WeakMap getOrInsertComputed callback is not callable",
                );
            }
            let Object::WeakMap { entries, .. } = self.heap.get(id)? else {
                unreachable!("receiver brand already checked");
            };
            if let Some(existing) = entries.get(&key) {
                return Ok(Invocation::Value(existing.clone()));
            }
            let inputs = (receiver, args);
            let roots = &roots.with(&inputs);
            let value = match self.invoke(
                host,
                roots,
                second,
                Arguments::Values(&args[..1]),
                &Value::UNDEFINED,
            )? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
            let Object::WeakMap { entries, .. } = self.heap.get_mut(id)? else {
                unreachable!("receiver brand already checked");
            };
            entries.insert(key, value.clone());
            return Ok(Invocation::Value(value));
        }
        let Object::WeakMap { entries, .. } = self.heap.get_mut(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(Invocation::Value(match method {
            WeakMapMethod::Get => entries.get(&key).cloned().unwrap_or(Value::UNDEFINED),
            WeakMapMethod::Set => {
                entries.insert(key, args.get(1).cloned().unwrap_or(Value::UNDEFINED));
                receiver.clone()
            }
            WeakMapMethod::GetOrInsert => {
                entries.entry(key).or_insert_with(|| second.clone()).clone()
            }
            WeakMapMethod::GetOrInsertComputed => unreachable!("handled above"),
            WeakMapMethod::Has => Value::boolean(entries.contains_key(&key)),
            WeakMapMethod::Delete => Value::boolean(entries.remove(&key).is_some()),
        }))
    }

    fn construct_weakset<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
        roots: &ActiveRoots<'_>,
        method: WeakSetMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "incompatible WeakSet receiver");
        };
        if !matches!(self.heap.get(id)?, Object::WeakSet { .. }) {
            return self.throw_type_error(roots, "incompatible WeakSet receiver");
        }
        let Some(key) = args.first().and_then(|value| self.heap.weak_key(value)) else {
            return match method {
                WeakSetMethod::Add => self.throw_type_error(roots, INVALID_WEAK_KEY),
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

    fn construct_collection<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn populate_collection<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
                return self.throw_type_error(roots, "collection adder is not callable");
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

    fn collection_add_item<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        entry_pairs: bool,
        collection: &Value,
        adder: &Value,
        item: Value,
    ) -> Result<Invocation, VmError> {
        let args = if entry_pairs {
            if !matches!(item.view(), ValueRef::Object(_)) {
                return self.throw_type_error(roots, "collection entry must be an object");
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
        self.invoke(host, roots, adder, Arguments::Values(&args), collection)
    }

    fn collection_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
            return self.throw_type_error(roots, invalid);
        };
        let object = self.heap.get(id)?;
        if !matches!(
            (kind, object),
            (CollectionKind::Map, Object::Map { .. }) | (CollectionKind::Set, Object::Set { .. })
        ) {
            return self.throw_type_error(roots, invalid);
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
                    return self
                        .throw_type_error(roots, "collection forEach callback is not callable");
                }
                let mut index = 0;
                while let Some((k, v)) = self.collection_pair(id, &mut index)? {
                    let result = self.invoke(
                        host,
                        roots,
                        key,
                        Arguments::Values(&[v, k, receiver.clone()]),
                        second,
                    )?;
                    if matches!(result, Invocation::Thrown(_)) {
                        return Ok(result);
                    }
                }
                Value::UNDEFINED
            }
            GetOrInsert => return self.map_get_or_insert(id, key, second),
            GetOrInsertComputed => {
                return self.map_get_or_insert_computed(host, roots, id, key, second);
            }
            _ => match self.heap.get_mut(id)? {
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

    /// Inserts without consulting script-replaceable `get` / `has` / `set`.
    fn map_get_or_insert(
        &mut self,
        id: ObjectId,
        key: &Value,
        value: &Value,
    ) -> Result<Invocation, VmError> {
        let key = canonical_key(key);
        let Object::Map { entries, .. } = self.heap.get_mut(id)? else {
            unreachable!("receiver brand already checked");
        };
        match entries
            .iter_mut()
            .flatten()
            .find(|(entry, _)| same_value_zero(entry, &key))
        {
            Some((_, existing)) => Ok(Invocation::Value(existing.clone())),
            None => {
                entries.push(Some((key, value.clone())));
                Ok(Invocation::Value(value.clone()))
            }
        }
    }

    fn map_get_or_insert_computed<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        key: &Value,
        callback: &Value,
    ) -> Result<Invocation, VmError> {
        if !self.is_callable(callback) {
            return self.throw_type_error(
                roots,
                "collection getOrInsertComputed callback is not callable",
            );
        }
        let key = canonical_key(key);
        let roots = &roots.with(&key);
        if let Some(existing) = self.map_live_value(id, &key)? {
            return Ok(Invocation::Value(existing));
        }
        let callback = callback.clone();
        let roots = &roots.with(&callback);
        let arguments = [key.clone()];
        let value = match self.invoke(
            host,
            roots,
            &callback,
            Arguments::Values(&arguments),
            &Value::UNDEFINED,
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        // The callback may have inserted the key; update in place to keep the slot position.
        let Object::Map { entries, .. } = self.heap.get_mut(id)? else {
            unreachable!("receiver brand already checked");
        };
        match entries
            .iter_mut()
            .flatten()
            .find(|(entry, _)| same_value_zero(entry, &key))
        {
            Some((_, existing)) => *existing = value.clone(),
            None => entries.push(Some((key, value.clone()))),
        }
        Ok(Invocation::Value(value))
    }

    fn map_live_value(&self, id: ObjectId, key: &Value) -> Result<Option<Value>, VmError> {
        let Object::Map { entries, .. } = self.heap.get(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(entries
            .iter()
            .flatten()
            .find(|(entry, _)| same_value_zero(entry, key))
            .map(|(_, value)| value.clone()))
    }

    fn map_group_by<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let undefined = Value::UNDEFINED;
        let items = args.first().unwrap_or(&undefined);
        if matches!(items.view(), ValueRef::Undefined | ValueRef::Null) {
            return self.throw_type_error(roots, "Map.groupBy requires a non-nullish value");
        }
        let callback = args.get(1).unwrap_or(&undefined);
        if !self.is_callable(callback) {
            return self.throw_type_error(roots, "Map.groupBy requires a callable callback");
        }
        let iterator_pair = match self.get_iterator(host, roots, items)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&iterator_pair);
        let (iterator, next) = &iterator_pair;
        let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
        let mut index = 0u64;
        loop {
            let roots = &roots.with(&groups);
            if index >= (1u64 << 53) - 1 {
                let error =
                    self.type_error(roots, "Map.groupBy exceeded the maximum group count")?;
                self.close_iterator(host, &roots.with(&error), iterator)?;
                return Ok(Invocation::Thrown(error));
            }
            let item = match self.iterator_step(host, roots, iterator, next)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&item);
            let call_args = [item.clone(), Value::number(index as f64)];
            let roots = &roots.with(&call_args);
            let key = match self.invoke(
                host,
                roots,
                callback,
                Arguments::Values(&call_args),
                &undefined,
            )? {
                Invocation::Value(key) => key,
                Invocation::Thrown(error) => {
                    self.close_iterator(host, &roots.with(&error), iterator)?;
                    return Ok(Invocation::Thrown(error));
                }
            };
            let key = canonical_key(&key);
            match groups
                .iter_mut()
                .find(|(existing, _)| same_value_zero(existing, &key))
            {
                Some((_, elements)) => elements.push(item),
                None => groups.push((key, vec![item])),
            }
            index += 1;
        }
        let prototype = self.builtins.get("map.prototype");
        let entries = groups
            .into_iter()
            .map(|(key, elements)| Some((key, Value::object(self.heap.alloc_array(elements)))))
            .collect();
        Ok(Invocation::Value(Value::object(self.heap.alloc(
            Object::Map {
                object: PlainObject::new(Some(prototype)),
                entries,
            },
        ))))
    }

    fn set_operation<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        operation: SetOperation,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let receiver_args = (receiver, args);
        let roots = &roots.with(&receiver_args);
        let invalid = "incompatible collection receiver";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid);
        };
        if !matches!(self.heap.get(id)?, Object::Set { .. }) {
            return self.throw_type_error(roots, invalid);
        }
        let undefined = Value::UNDEFINED;
        let other = args.first().unwrap_or(&undefined);
        let record = match self.get_set_record(host, roots, other)? {
            Ok(record) => record,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let record_view = (&record.object, &record.has, &record.keys);
        let roots = &roots.with(&record_view);
        match operation {
            SetOperation::Union => self.set_union(host, roots, id, &record),
            SetOperation::Intersection => self.set_intersection(host, roots, id, &record),
            SetOperation::Difference => self.set_difference(host, roots, id, &record),
            SetOperation::SymmetricDifference => {
                self.set_symmetric_difference(host, roots, id, &record)
            }
            SetOperation::IsSubsetOf => self.set_is_subset_of(host, roots, id, &record),
            SetOperation::IsSupersetOf => self.set_is_superset_of(host, roots, id, &record),
            SetOperation::IsDisjointFrom => self.set_is_disjoint_from(host, roots, id, &record),
        }
    }

    fn get_set_record<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        other: &Value,
    ) -> Result<Result<SetRecord, Value>, VmError> {
        if !matches!(other.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "set operation requires an object argument")?
            ));
        }
        let roots = &roots.with(other);
        let size = match self.read_property(host, roots, other, PropertyKeyRef::String("size"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&size);
        let size = match self.coerce_number(host, roots, &size)? {
            Ok(size) => size,
            Err(error) => return Ok(Err(error)),
        };
        if size.is_nan() {
            return Ok(Err(self.type_error(roots, "set-like size must not be NaN")?));
        }
        let size = size.trunc();
        if size < 0.0 {
            return Ok(Err(
                self.make_range_error(roots, "set-like size must not be negative")?
            ));
        }
        let has = match self.read_property(host, roots, other, PropertyKeyRef::String("has"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&has);
        if !self.is_callable(&has) {
            return Ok(Err(self.type_error(roots, "set-like has must be callable")?));
        }
        let keys = match self.read_property(host, roots, other, PropertyKeyRef::String("keys"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&keys);
        if !self.is_callable(&keys) {
            return Ok(Err(
                self.type_error(roots, "set-like keys must be callable")?
            ));
        }
        Ok(Ok(SetRecord {
            object: other.clone(),
            size,
            has,
            keys,
        }))
    }

    fn set_elements_copy(&self, id: ObjectId) -> Result<Vec<Option<Value>>, VmError> {
        let Object::Set { elements, .. } = self.heap.get(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(elements.clone())
    }

    fn set_live_count(&self, id: ObjectId) -> Result<usize, VmError> {
        let Object::Set { elements, .. } = self.heap.get(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(elements.iter().flatten().count())
    }

    fn set_live_contains(&self, id: ObjectId, value: &Value) -> Result<bool, VmError> {
        let Object::Set { elements, .. } = self.heap.get(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(live_contains(elements, value))
    }

    /// Clones the value at `index`, then advances past it. Appended slots stay reachable.
    fn set_next_element(&self, id: ObjectId, index: &mut usize) -> Result<Option<Value>, VmError> {
        let Object::Set { elements, .. } = self.heap.get(id)? else {
            unreachable!("receiver brand already checked");
        };
        Ok(next_slot(elements, index).cloned())
    }

    fn alloc_set(&mut self, elements: Vec<Option<Value>>) -> Value {
        let prototype = self.builtins.get("set.prototype");
        Value::object(self.heap.alloc(Object::Set {
            object: PlainObject::new(Some(prototype)),
            elements,
        }))
    }

    fn call_set_has<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        record: &SetRecord,
        value: &Value,
    ) -> Result<Result<bool, Value>, VmError> {
        let arguments = [value.clone()];
        let root_view = (&record.object, &record.has, &arguments);
        let roots = &roots.with(&root_view);
        match self.invoke(
            host,
            roots,
            &record.has,
            Arguments::Values(&arguments),
            &record.object,
        )? {
            Invocation::Value(result) => Ok(Ok(result.truthy())),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn set_keys_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        record: &SetRecord,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        self.get_iterator_from_method(host, roots, &record.object, &record.keys)
    }

    fn set_union<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        let pair = match self.set_keys_iterator(host, roots, record)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&pair);
        let mut result = self.set_elements_copy(id)?;
        loop {
            let roots = &roots.with(&result);
            let item = match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let item = canonical_key(&item);
            if !live_contains(&result, &item) {
                result.push(Some(item));
            }
        }
        Ok(Invocation::Value(self.alloc_set(result)))
    }

    fn set_intersection<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        let mut result: Vec<Option<Value>> = Vec::new();
        if (self.set_live_count(id)? as f64) <= record.size {
            let mut index = 0;
            while let Some(entry) = self.set_next_element(id, &mut index)? {
                let root_view = (&entry, &result);
                let roots = &roots.with(&root_view);
                let in_other = match self.call_set_has(host, roots, record, &entry)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if in_other && !live_contains(&result, &entry) {
                    result.push(Some(entry));
                }
            }
        } else {
            let pair = match self.set_keys_iterator(host, roots, record)? {
                Ok(pair) => pair,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&pair);
            loop {
                let item = {
                    let roots = &roots.with(&result);
                    match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                        Ok(Some(item)) => item,
                        Ok(None) => break,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                let item = canonical_key(&item);
                if self.set_live_contains(id, &item)? && !live_contains(&result, &item) {
                    result.push(Some(item));
                }
            }
        }
        Ok(Invocation::Value(self.alloc_set(result)))
    }

    fn set_difference<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        let mut result = self.set_elements_copy(id)?;
        if (self.set_live_count(id)? as f64) <= record.size {
            let mut index = 0;
            while let Some(entry) = next_slot(&result, &mut index).cloned() {
                let root_view = (&entry, &result);
                let roots = &roots.with(&root_view);
                let in_other = match self.call_set_has(host, roots, record, &entry)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if in_other {
                    live_remove(&mut result, &entry);
                }
            }
        } else {
            let pair = match self.set_keys_iterator(host, roots, record)? {
                Ok(pair) => pair,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&pair);
            loop {
                let item = {
                    let roots = &roots.with(&result);
                    match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                        Ok(Some(item)) => item,
                        Ok(None) => break,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                let item = canonical_key(&item);
                live_remove(&mut result, &item);
            }
        }
        Ok(Invocation::Value(self.alloc_set(result)))
    }

    fn set_symmetric_difference<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        let pair = match self.set_keys_iterator(host, roots, record)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut result = self.set_elements_copy(id)?;
        let roots = &roots.with(&pair);
        loop {
            let item = {
                let roots = &roots.with(&result);
                match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                    Ok(Some(item)) => item,
                    Ok(None) => break,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            };
            let item = canonical_key(&item);
            let position = live_position(&result, &item);
            if self.set_live_contains(id, &item)? {
                if let Some(position) = position {
                    result[position] = None;
                }
            } else if position.is_none() {
                result.push(Some(item));
            }
        }
        Ok(Invocation::Value(self.alloc_set(result)))
    }

    fn set_is_subset_of<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        if (self.set_live_count(id)? as f64) > record.size {
            return Ok(Invocation::Value(Value::boolean(false)));
        }
        let mut index = 0;
        while let Some(entry) = self.set_next_element(id, &mut index)? {
            let roots = &roots.with(&entry);
            let in_other = match self.call_set_has(host, roots, record, &entry)? {
                Ok(value) => value,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if !in_other {
                return Ok(Invocation::Value(Value::boolean(false)));
            }
        }
        Ok(Invocation::Value(Value::boolean(true)))
    }

    fn set_is_superset_of<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        if (self.set_live_count(id)? as f64) < record.size {
            return Ok(Invocation::Value(Value::boolean(false)));
        }
        let pair = match self.set_keys_iterator(host, roots, record)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&pair);
        loop {
            let item = match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if !self.set_live_contains(id, &item)? {
                return match self.close_iterator(host, roots, &pair.0)? {
                    Invocation::Value(_) => Ok(Invocation::Value(Value::boolean(false))),
                    Invocation::Thrown(error) => Ok(Invocation::Thrown(error)),
                };
            }
        }
        Ok(Invocation::Value(Value::boolean(true)))
    }

    fn set_is_disjoint_from<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        record: &SetRecord,
    ) -> Result<Invocation, VmError> {
        if (self.set_live_count(id)? as f64) <= record.size {
            let mut index = 0;
            while let Some(entry) = self.set_next_element(id, &mut index)? {
                let roots = &roots.with(&entry);
                let in_other = match self.call_set_has(host, roots, record, &entry)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if in_other {
                    return Ok(Invocation::Value(Value::boolean(false)));
                }
            }
        } else {
            let pair = match self.set_keys_iterator(host, roots, record)? {
                Ok(pair) => pair,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&pair);
            loop {
                let item = match self.iterator_step(host, roots, &pair.0, &pair.1)? {
                    Ok(Some(item)) => item,
                    Ok(None) => break,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if self.set_live_contains(id, &item)? {
                    return match self.close_iterator(host, roots, &pair.0)? {
                        Invocation::Value(_) => Ok(Invocation::Value(Value::boolean(false))),
                        Invocation::Thrown(error) => Ok(Invocation::Thrown(error)),
                    };
                }
            }
        }
        Ok(Invocation::Value(Value::boolean(true)))
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
        roots: &ActiveRoots<'_>,
        kind: CollectionKind,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid = "incompatible collection iterator receiver";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid);
        };
        let Object::CollectionIterator {
            kind: actual,
            target,
            iteration,
            next_index,
            ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(roots, invalid);
        };
        if *actual != kind {
            return self.throw_type_error(roots, invalid);
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

fn live_position(slots: &[Option<Value>], value: &Value) -> Option<usize> {
    slots.iter().position(|slot| {
        slot.as_ref()
            .is_some_and(|entry| same_value_zero(entry, value))
    })
}

fn live_contains(slots: &[Option<Value>], value: &Value) -> bool {
    live_position(slots, value).is_some()
}

fn live_remove(slots: &mut [Option<Value>], value: &Value) -> bool {
    let Some(position) = live_position(slots, value) else {
        return false;
    };
    slots[position] = None;
    true
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

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
