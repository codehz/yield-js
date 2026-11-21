//! Builtin roots are allocated and restored from the same owner declarations.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::VmError;
use crate::heap::{Attributes, Heap, NativeObject, Object, Property};
use crate::intrinsic::IntrinsicKey;
use crate::value::{ObjectId, PropertyKeyRef, Value};
use crate::vm::Vm;

pub(crate) enum RootKind {
    Plain,
    Array,
    Native(IntrinsicKey),
}

pub(crate) struct RootSpec {
    name: &'static str,
    kind: RootKind,
    parent: Option<&'static str>,
    fixed_parent: bool,
    fixed_prototype: Option<&'static str>,
    backlink: Option<Option<Attributes>>,
}

impl RootSpec {
    pub const fn new(name: &'static str, kind: RootKind, parent: Option<&'static str>) -> Self {
        Self {
            name,
            kind,
            parent,
            fixed_parent: false,
            fixed_prototype: None,
            backlink: None,
        }
    }

    pub const fn fixed_parent(self) -> Self {
        self.with_fixed_parent(true)
    }

    pub const fn with_fixed_parent(mut self, fixed: bool) -> Self {
        self.fixed_parent = fixed;
        self
    }

    pub const fn fixed(mut self, prototype: &'static str) -> Self {
        self.fixed_prototype = Some(prototype);
        self
    }

    pub const fn backlink(mut self, attributes: Option<Attributes>) -> Self {
        self.backlink = Some(attributes);
        self
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BuiltinRoots(BTreeMap<&'static str, ObjectId>);

impl BuiltinRoots {
    pub fn get(&self, key: &'static str) -> ObjectId {
        self.0[key]
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.0.values().copied()
    }

    pub fn to_map(&self) -> BTreeMap<String, ObjectId> {
        self.0
            .iter()
            .map(|(&key, &id)| (key.to_owned(), id))
            .collect()
    }

    pub fn load(roots: &BTreeMap<String, ObjectId>, heap: &Heap) -> Result<Self, VmError> {
        let specs = || crate::builtin::globals::root_groups().flatten();
        let expected = specs().count();
        if roots.len() != expected {
            return Err(VmError::InvalidSnapshot(format!(
                "snapshot has {} builtin roots, expected {expected}",
                roots.len()
            )));
        }
        let mut ids = BTreeSet::new();
        let mut result = Self::default();
        for spec in specs() {
            let name = spec.name;
            let id = *roots
                .get(name)
                .ok_or_else(|| VmError::InvalidSnapshot(format!("missing builtin root {name}")))?;
            let valid = match (&spec.kind, heap.get(id)?) {
                (RootKind::Plain, Object::Plain(_)) | (RootKind::Array, Object::Array(_)) => true,
                (
                    RootKind::Native(key),
                    Object::Native(NativeObject::Intrinsic { intrinsic, .. }),
                ) => *intrinsic == key.link(),
                _ => false,
            };
            if !valid {
                return Err(VmError::InvalidSnapshot(format!(
                    "{name} builtin root has invalid type or target"
                )));
            }
            if !ids.insert(id) {
                return Err(VmError::InvalidSnapshot(format!(
                    "{name} aliases another builtin root"
                )));
            }
            result.0.insert(name, id);
        }
        for spec in specs() {
            let id = result.get(spec.name);
            if spec.fixed_parent
                && heap.get(id)?.prototype() != spec.parent.map(|parent| result.get(parent))
            {
                let detail = if spec.parent.is_none() {
                    "Object.prototype must have a null prototype".to_owned()
                } else {
                    format!("{} prototype has invalid inheritance", spec.name)
                };
                return Err(VmError::InvalidSnapshot(detail));
            }
            if let Some(key) = spec.fixed_prototype {
                let prototype = result.get(key);
                let valid = heap
                    .get(id)?
                    .own_property(PropertyKeyRef::String("prototype"))
                    .is_some_and(|prop| {
                        prop.attributes() == Attributes::FIXED
                            && prop.into_owned() == Property::Data(Value::object(prototype))
                    });
                if !valid {
                    let name = match spec.kind {
                        RootKind::Native(key) => key.owner,
                        _ => spec.name,
                    };
                    return Err(VmError::InvalidSnapshot(format!(
                        "{name}.prototype is invalid"
                    )));
                }
                if let Some(attributes) = spec.backlink {
                    let valid = heap
                        .get(prototype)?
                        .own_property(PropertyKeyRef::String("constructor"))
                        .is_some_and(|prop| {
                            attributes.is_none_or(|attributes| prop.attributes() == attributes)
                                && prop.into_owned() == Property::Data(Value::object(id))
                        });
                    if !valid {
                        return Err(VmError::InvalidSnapshot(format!(
                            "{key} constructor is invalid"
                        )));
                    }
                }
            }
        }
        let function_proto = heap.get(result.get("function.prototype"))?;
        let has_instance =
            function_proto.own_property(PropertyKeyRef::Symbol(crate::heap::HAS_INSTANCE_SYMBOL));
        if !has_instance.is_some_and(|prop| {
            prop.attributes() == Attributes::FIXED
                && matches!(prop.data_value().map(Value::view), Some(crate::value::ValueRef::Object(id))
                    if matches!(heap.get(id), Ok(Object::Native(NativeObject::Intrinsic { intrinsic, .. }))
                        if *intrinsic == crate::exec::FUNCTION_HAS_INSTANCE.link()))
        }) {
            return Err(VmError::InvalidSnapshot("invalid Function.prototype[Symbol.hasInstance]".into()));
        }
        Ok(result)
    }
}

impl Vm {
    pub(crate) fn allocate_builtin_roots(&mut self) {
        let specs = || crate::builtin::globals::root_groups().flatten();
        for spec in specs() {
            let id = match spec.kind {
                RootKind::Plain => self.heap.alloc_plain_with(None),
                RootKind::Array => self.heap.alloc_array(Vec::new()),
                RootKind::Native(key) => self.alloc_intrinsic(key),
            };
            assert!(
                self.builtins.0.insert(spec.name, id).is_none(),
                "duplicate builtin root {}",
                spec.name
            );
            match spec.name {
                "object.prototype" => self.heap.object_prototype = Some(id),
                "function.prototype" => self.heap.function_prototype = Some(id),
                "array.prototype" => self.heap.array_prototype = Some(id),
                "promise.prototype" => self.heap.promise_prototype = Some(id),
                _ => {}
            }
        }
        for spec in specs() {
            let id = self.builtins.get(spec.name);
            let parent = spec.parent.map(|name| self.builtins.get(name));
            assert!(
                self.heap
                    .set_prototype(id, parent)
                    .expect("builtin root prototype")
            );
        }
    }
}
