//! Process-local linkage for engine-owned native functions.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::binding::NativeTarget;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

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

    pub fn link(self) -> IntrinsicId {
        table().ids[&(self.owner, self.slot, self.member)]
    }
}

type Call =
    fn(&mut Vm, &mut dyn Host, &ActiveRoots<'_>, Vec<Value>, &Value) -> Result<Invocation, VmError>;
type Construct = fn(
    &mut Vm,
    &mut dyn Host,
    &ActiveRoots<'_>,
    Vec<Value>,
    Option<ObjectId>,
) -> Result<Invocation, VmError>;

pub(crate) struct IntrinsicDef {
    pub key: IntrinsicKey,
    pub call: Call,
    pub construct: Option<Construct>,
    pub skip_new_target_prototype: bool,
}

impl IntrinsicDef {
    pub const fn call(key: IntrinsicKey, call: Call) -> Self {
        Self {
            key,
            call,
            construct: None,
            skip_new_target_prototype: false,
        }
    }

    pub const fn constructor(key: IntrinsicKey, call: Call, construct: Construct) -> Self {
        Self {
            construct: Some(construct),
            ..Self::call(key, call)
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
    pub(crate) fn definition(self) -> &'static IntrinsicDef {
        table().definitions[self.0 as usize]
    }
}

impl Serialize for IntrinsicId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.definition().key.serialize(serializer)
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
    definitions: Vec<&'static IntrinsicDef>,
    ids: BTreeMap<LookupKey<'static>, IntrinsicId>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table {
            definitions: Vec::new(),
            ids: BTreeMap::new(),
        };
        for def in crate::builtin::globals::intrinsic_groups().flatten() {
            let id =
                IntrinsicId(u32::try_from(table.definitions.len()).expect("intrinsic ID overflow"));
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
            table.definitions.push(def);
        }
        table
    })
}

impl Vm {
    pub(crate) fn alloc_intrinsic(&mut self, key: IntrinsicKey) -> ObjectId {
        self.heap.alloc_intrinsic(key.link())
    }

    pub(crate) fn with_result_prototype(
        &mut self,
        result: Invocation,
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        if let (Some(prototype), Invocation::Value(value)) = (prototype, &result)
            && let ValueRef::Object(id) = value.view()
            && !self.heap.set_prototype(id, Some(prototype))?
        {
            return self.throw_type_error("cannot set constructor result prototype");
        }
        Ok(result)
    }
}
