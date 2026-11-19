use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bytecode::BYTECODE_VERSION;
use crate::heap::Object;
use crate::value::{ContinuationId, ObjectId, Value};

pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub id: ObjectId,
    pub object: Object,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationRecord {
    pub id: ContinuationId,
    /// Async-function result Promise.
    pub async_promise: ObjectId,
    pub func_id: u32,
    pub resume_id: u32,
    /// Promise the continuation waits on. Only recoverable, unsettled promises
    /// are persisted, so this is required for every restored continuation.
    pub waiting_on: ObjectId,
    /// Values in `ResumeEntry::saved_slots` order.
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOperationRecord {
    pub id: String,
    pub import: String,
    pub promise: ObjectId,
}

/// Host-held script roots live outside the VM. The snapshot deliberately does
/// not contain native pointers, host callbacks, or pending microtasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format_version: u32,
    pub bytecode_version: u32,
    pub build_id: String,
    pub objects: Vec<ObjectRecord>,
    pub continuations: Vec<ContinuationRecord>,
    pub operations: Vec<PendingOperationRecord>,
    pub globals: BTreeMap<String, Value>,
}

impl Snapshot {
    pub fn new(build_id: String) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            bytecode_version: BYTECODE_VERSION,
            build_id,
            objects: Vec::new(),
            continuations: Vec::new(),
            operations: Vec::new(),
            globals: BTreeMap::new(),
        }
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(source: &str) -> Result<Self, String> {
        serde_json::from_str(source).map_err(|error| error.to_string())
    }
}
