//! Process-local linkage for engine-owned native functions.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::binding::NativeTarget;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::host::{Host, Platform, PlatformServices};
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Slot {
    Global,
    Static,
    Prototype,
    StaticSymbol,
    PrototypeSymbol,
    StaticGetter,
    PrototypeGetter,
    StaticSetter,
    PrototypeSetter,
    StaticSymbolGetter,
    PrototypeSymbolGetter,
    StaticSymbolSetter,
    PrototypeSymbolSetter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct IntrinsicKey {
    pub owner: &'static str,
    pub slot: Slot,
    pub member: &'static str,
}

impl IntrinsicKey {
    pub const fn new(owner: &'static str, slot: Slot, member: &'static str) -> Self {
        Self {
            owner,
            slot,
            member,
        }
    }

    pub fn initial_name(self) -> crate::value::JsString {
        use Slot::*;
        if self.slot == Global {
            return self.owner.into();
        }
        let prefix = match self.slot {
            StaticGetter | PrototypeGetter | StaticSymbolGetter | PrototypeSymbolGetter => "get ",
            StaticSetter | PrototypeSetter | StaticSymbolSetter | PrototypeSymbolSetter => "set ",
            _ => "",
        };
        if matches!(
            self.slot,
            StaticSymbol
                | PrototypeSymbol
                | StaticSymbolGetter
                | PrototypeSymbolGetter
                | StaticSymbolSetter
                | PrototypeSymbolSetter
        ) {
            format!("{prefix}[Symbol.{}]", self.member).into()
        } else if prefix.is_empty() {
            self.member.into()
        } else {
            format!("{prefix}{}", self.member).into()
        }
    }

    pub fn link(self) -> IntrinsicId {
        table().ids[&(self.owner, self.slot, self.member)]
    }
}

type Call<P> =
    fn(&mut VmCore, &Host<P>, &ActiveRoots<'_>, Vec<Value>, &Value) -> Result<Invocation, VmError>;
type Construct<P> = fn(
    &mut VmCore,
    &Host<P>,
    &ActiveRoots<'_>,
    Vec<Value>,
    Option<ObjectId>,
) -> Result<Invocation, VmError>;
type ConstructWithTarget<P> =
    fn(&mut VmCore, &Host<P>, &ActiveRoots<'_>, Vec<Value>, &Value) -> Result<Invocation, VmError>;

#[derive(Clone, Copy)]
pub(crate) enum ClassArgs<'a> {
    Values(&'a [Value]),
    Handles(&'a [crate::JsValue]),
}

impl ClassArgs<'_> {
    pub(crate) fn get(&self, index: usize) -> Option<&Value> {
        match self {
            Self::Values(values) => values.get(index),
            Self::Handles(values) => values.get(index).map(|arg| &arg.value),
        }
    }
}

impl crate::gc::TraceRoots for ClassArgs<'_> {
    fn trace(&self, tracer: &mut crate::gc::Tracer<'_>) -> Result<(), VmError> {
        match self {
            Self::Values(values) => values.trace(tracer),
            Self::Handles(values) => values.trace(tracer),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ClassPrototype<'a> {
    Resolved(ObjectId),
    // DOMException coerces its Web IDL arguments before reading the prototype.
    Deferred(&'a Value),
}

impl ClassPrototype<'_> {
    pub(crate) fn resolved(self) -> ObjectId {
        let Self::Resolved(id) = self else {
            unreachable!("class requires resolved prototype")
        };
        id
    }
}

pub(crate) type InitializeClass<P> = fn(
    &mut VmCore,
    &Host<P>,
    &ActiveRoots<'_>,
    ClassPrototype<'_>,
    &str,
    ClassArgs<'_>,
) -> Result<Invocation, VmError>;

/// A class's stable roots and instance shape, shared across platforms.
#[derive(Debug)]
pub(crate) struct ClassDef {
    pub root: &'static str,
    pub prototype: &'static str,
    pub key: IntrinsicKey,
    pub valid_instance: fn(&crate::heap::Object) -> bool,
    pub instance_description: &'static str,
}

impl ClassDef {
    pub(crate) fn constructor(&self, vm: &VmCore) -> Value {
        Value::object(vm.builtins.get(self.root))
    }

    pub(crate) fn initialize<P: PlatformServices>(
        &self,
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        name: &str,
        args: ClassArgs<'_>,
    ) -> Result<Invocation, VmError> {
        let id = vm.builtins.get(self.root);
        let crate::heap::Object::Native(crate::heap::NativeObject::Intrinsic { intrinsic, .. }) =
            vm.heap.get(id)?
        else {
            unreachable!("intrinsic constructor root is not native")
        };
        let Some(Construction::Class(_, initialize)) = intrinsic.definition::<P>().construct else {
            unreachable!("registered intrinsic class without initializer")
        };
        initialize(
            vm,
            host,
            &roots.with(&(&prototype, &args)),
            ClassPrototype::Resolved(prototype),
            name,
            args,
        )
    }
}

pub(crate) enum Construction<P: PlatformServices> {
    WithPrototype(Construct<P>),
    WithNewTarget(ConstructWithTarget<P>),
    Class(&'static ClassDef, InitializeClass<P>),
}

impl<P: PlatformServices> Copy for Construction<P> {}

impl<P: PlatformServices> Clone for Construction<P> {
    fn clone(&self) -> Self {
        *self
    }
}

pub(crate) struct IntrinsicDef<P: PlatformServices> {
    pub key: IntrinsicKey,
    pub length: u32,
    pub call: Call<P>,
    pub construct: Option<Construction<P>>,
    pub skip_new_target_prototype: bool,
}

impl<P: PlatformServices> IntrinsicDef<P> {
    pub const fn call(key: IntrinsicKey, length: u32, call: Call<P>) -> Self {
        Self {
            key,
            length,
            call,
            construct: None,
            skip_new_target_prototype: false,
        }
    }

    pub const fn constructor(
        key: IntrinsicKey,
        length: u32,
        call: Call<P>,
        construct: Construct<P>,
    ) -> Self {
        Self {
            construct: Some(Construction::WithPrototype(construct)),
            ..Self::call(key, length, call)
        }
    }

    pub const fn class(
        class: &'static ClassDef,
        length: u32,
        call: Call<P>,
        initialize: InitializeClass<P>,
    ) -> Self {
        Self {
            construct: Some(Construction::Class(class, initialize)),
            ..Self::call(class.key, length, call)
        }
    }

    /// The constructor reads newTarget.prototype itself, after coercion.
    pub const fn constructor_with_target(
        key: IntrinsicKey,
        length: u32,
        call: Call<P>,
        construct: ConstructWithTarget<P>,
    ) -> Self {
        Self {
            construct: Some(Construction::WithNewTarget(construct)),
            ..Self::call(key, length, call).without_new_target_prototype()
        }
    }

    pub const fn without_new_target_prototype(mut self) -> Self {
        self.skip_new_target_prototype = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IntrinsicId(u32);

impl IntrinsicId {
    pub(crate) fn definition<P: PlatformServices>(self) -> &'static IntrinsicDef<P> {
        // Every specialization is built from the same ordered groups.
        let (group, member) = table().positions[self.0 as usize];
        &crate::builtin::globals::BuiltinTable::<P>::groups()[group][member]
    }
}

impl Serialize for IntrinsicId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.definition::<Platform>().key.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IntrinsicId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Key {
            owner: String,
            slot: Slot,
            member: String,
        }
        let Key {
            owner,
            slot,
            member,
        } = Key::deserialize(deserializer)?;
        table()
            .ids
            .get(&(owner.as_str(), slot, member.as_str()))
            .copied()
            .ok_or_else(|| {
                serde::de::Error::custom(format_args!(
                    "unknown intrinsic key {owner}:{slot:?}:{member}"
                ))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallableTarget {
    Host(NativeTarget),
    Intrinsic(IntrinsicId),
}

type LookupKey<'a> = (&'a str, Slot, &'a str);

struct Table {
    positions: Vec<(usize, usize)>,
    ids: BTreeMap<LookupKey<'static>, IntrinsicId>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table {
            positions: Vec::new(),
            ids: BTreeMap::new(),
        };
        for (group, definitions) in crate::builtin::globals::BuiltinTable::<Platform>::groups()
            .iter()
            .enumerate()
        {
            for (index, def) in definitions.iter().enumerate() {
                let id = IntrinsicId(
                    u32::try_from(table.positions.len()).expect("intrinsic ID overflow"),
                );
                let IntrinsicKey {
                    owner,
                    slot,
                    member,
                } = def.key;
                assert!(
                    table.ids.insert((owner, slot, member), id).is_none(),
                    "duplicate intrinsic key: {:?}",
                    def.key
                );
                table.positions.push((group, index));
            }
        }
        table
    })
}

impl VmCore {
    pub(crate) fn alloc_intrinsic(&mut self, key: IntrinsicKey) -> ObjectId {
        self.heap.alloc_intrinsic(key.link())
    }

    pub(crate) fn with_result_prototype(
        &mut self,
        roots: &ActiveRoots<'_>,
        result: Invocation,
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        if let (Some(prototype), Invocation::Value(value)) = (prototype, &result)
            && let ValueRef::Object(id) = value.view()
            && !self.heap.set_prototype(id, Some(prototype))?
        {
            return self.throw_type_error(roots, "cannot set constructor result prototype");
        }
        Ok(result)
    }
}
