use std::collections::BTreeMap;

use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::JsString;
use crate::binding::{NativeTarget, Recovery};
use crate::bytecode::BYTECODE_VERSION;
use crate::heap::{CombineState, GeneratorObject, Object};
use crate::value::{ContinuationId, ObjectId, SymbolId, Value};

// Global numeric, URI, and eval function roots.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 66;

/// Version for the binary envelope and bincode options, not the snapshot schema.
pub const SNAPSHOT_BINARY_VERSION: u32 = 1;
const SNAPSHOT_MAGIC: &[u8; 8] = b"YIELDSNP";
const SNAPSHOT_HEADER_LEN: usize = SNAPSHOT_MAGIC.len() + 2 * size_of::<u32>();

fn snapshot_codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .with_little_endian()
        .reject_trailing_bytes()
}

pub(crate) mod ordered_pairs {
    use std::collections::BTreeMap;
    use std::collections::btree_map::Entry;
    use std::fmt;
    use std::marker::PhantomData;

    use serde::de::{Error, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<K: Ord + Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map)
    }

    pub(crate) fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Ord + Deserialize<'de>,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        struct Pairs<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for Pairs<K, V>
        where
            K: Ord + Deserialize<'de>,
            V: Deserialize<'de>,
        {
            type Value = BTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of key-value pairs")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = sequence.next_element()? {
                    match map.entry(key) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(_) => {
                            return Err(A::Error::custom("duplicate key in ordered pairs"));
                        }
                    }
                }
                Ok(map)
            }
        }

        deserializer.deserialize_seq(Pairs(PhantomData))
    }
}

pub(super) fn valid_string_position(string: &JsString, position: usize) -> bool {
    position <= string.unit_len()
        && (position == 0
            || !(matches!(string.unit_at(position - 1), Some(0xd800..=0xdbff))
                && matches!(string.unit_at(position), Some(0xdc00..=0xdfff))))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub id: ObjectId,
    pub object: Object,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationRecord {
    pub id: ContinuationId,
    /// What the resumed step delivers its outcome to: an async activation's
    /// result Promise, or the generator whose request is still waiting.
    pub target: ContinuationTargetRecord,
    pub func_id: u32,
    pub resume_id: u32,
    /// Promise the continuation waits on. Only recoverable, unsettled promises
    /// are persisted, so this is required for every restored continuation.
    pub waiting_on: ObjectId,
    /// Invocation environment of the suspended frame. Required exactly when
    /// [`crate::bytecode::Function::needs_environment`] is true.
    pub env: Option<ObjectId>,
    /// Values in `ResumeEntry::saved_slots` order.
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationTargetRecord {
    Promise(ObjectId),
    Generator(ObjectId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTarget {
    Promise(ObjectId),
    Task(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOperationRecord {
    pub id: u64,
    pub name: String,
    pub schema: u32,
    pub recovery: Recovery,
    pub state: Vec<u8>,
    pub target: DeliveryTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostTaskRecord {
    pub id: u64,
    pub owner: NativeTarget,
    pub stage: u32,
    pub receiver: Value,
    pub saved: Vec<Value>,
    pub waiting_on: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSlotRecord {
    Js(Value),
    JsList(Vec<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRecord {
    pub schema: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInstanceRecord {
    pub object: ObjectId,
    pub class: String,
    pub slots: Vec<HostSlotRecord>,
    pub payload: Option<PayloadRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleStateRecord {
    pub module: String,
    pub slots: Vec<HostSlotRecord>,
    pub payload: PayloadRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PrivateRecord {
    pub receiver: ObjectId,
    pub brand: ObjectId,
    pub index: u32,
    pub element: crate::heap::PrivateElement,
}

/// Host-held script roots live outside the VM. The snapshot deliberately does
/// not contain native pointers, host callbacks, or pending microtasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format_version: u32,
    pub bytecode_version: u32,
    pub build_id: String,
    pub binding_fingerprint: String,
    pub objects: Vec<ObjectRecord>,
    pub(crate) private_elements: Vec<PrivateRecord>,
    pub continuations: Vec<ContinuationRecord>,
    pub operations: Vec<PendingOperationRecord>,
    pub host_tasks: Vec<HostTaskRecord>,
    pub next_host_task_id: u64,
    /// Promises awaiting rejection notifications, in delivery order.
    pub pending_rejection_notifications: Vec<ObjectId>,
    #[serde(with = "ordered_pairs")]
    pub globals: BTreeMap<JsString, Value>,
    pub roots: BTreeMap<String, ObjectId>,
    /// Frozen template objects aligned with the build's template call sites.
    pub templates: Vec<ObjectId>,
    /// VM-managed module namespaces corresponding to the sorted build manifest.
    pub imports: Vec<ObjectId>,
    /// Class tag to the VM-owned prototype saved with the object graph.
    pub class_prototypes: BTreeMap<String, ObjectId>,
    pub host_instances: Vec<HostInstanceRecord>,
    pub module_states: Vec<ModuleStateRecord>,
    /// Source module environments, in build module order.
    pub module_envs: Vec<Option<ObjectId>>,
    /// Source module namespaces, in build module order.
    pub module_namespaces: Vec<ObjectId>,
    pub combines: BTreeMap<ObjectId, CombineState>,
    /// Suspended generator frames, keyed by their ordinary generator object.
    pub generators: BTreeMap<ObjectId, GeneratorObject>,
    pub random_state: u64,
    /// Descriptions of the symbols the snapshot keeps alive, keyed by id.
    pub symbols: BTreeMap<SymbolId, Option<JsString>>,
    /// `Symbol.for` registry entries kept alive by the snapshot.
    #[serde(with = "ordered_pairs")]
    pub symbol_registry: BTreeMap<JsString, SymbolId>,
    /// Allocation high-water mark, preserved so restored ids stay fresh.
    pub next_symbol_id: SymbolId,
}

impl Snapshot {
    pub fn new(build_id: String) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            bytecode_version: BYTECODE_VERSION,
            build_id,
            binding_fingerprint: String::new(),
            objects: Vec::new(),
            private_elements: Vec::new(),
            continuations: Vec::new(),
            operations: Vec::new(),
            host_tasks: Vec::new(),
            next_host_task_id: 1,
            pending_rejection_notifications: Vec::new(),
            imports: Vec::new(),
            class_prototypes: BTreeMap::new(),
            host_instances: Vec::new(),
            module_states: Vec::new(),
            globals: BTreeMap::new(),
            roots: BTreeMap::new(),
            templates: Vec::new(),
            module_envs: Vec::new(),
            module_namespaces: Vec::new(),
            combines: BTreeMap::new(),
            generators: BTreeMap::new(),
            random_state: crate::builtin::math::RANDOM_SEED,
            symbols: BTreeMap::new(),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: 0,
        }
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(source: &str) -> Result<Self, String> {
        serde_json::from_str(source).map_err(|error| error.to_string())
    }

    /// Encode a snapshot in the versioned binary container. This does not
    /// verify that the snapshot can be restored against a particular build.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SNAPSHOT_MAGIC);
        bytes.extend_from_slice(&SNAPSHOT_BINARY_VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.format_version.to_le_bytes());
        snapshot_codec()
            .serialize_into(&mut bytes, self)
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    /// Decode the binary container without authenticating or auditing its contents.
    /// VM restore still checks exact build and host compatibility.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let Some((header, payload)) = bytes.split_at_checked(SNAPSHOT_HEADER_LEN) else {
            return Err("binary snapshot is too short".to_owned());
        };
        if !header.starts_with(SNAPSHOT_MAGIC) {
            return Err("invalid binary snapshot magic".to_owned());
        }
        let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
        if version != SNAPSHOT_BINARY_VERSION {
            return Err(format!("unsupported binary snapshot version {version}"));
        }
        let format = u32::from_le_bytes(header[12..16].try_into().unwrap());
        if format != SNAPSHOT_FORMAT_VERSION {
            return Err(format!("unsupported snapshot format {format}"));
        }
        let snapshot: Self = snapshot_codec()
            .deserialize(payload)
            .map_err(|error| format!("invalid binary snapshot: {error}"))?;
        if snapshot.format_version != format {
            return Err("binary snapshot header does not match payload format".to_owned());
        }
        Ok(snapshot)
    }
}
