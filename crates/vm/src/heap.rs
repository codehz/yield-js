use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::value::{BindingId, ContinuationId, NativeKey, ObjectId, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromiseObject {
    pub state: PromiseState,
    /// Continuations waiting for settlement. The waiting relation is stored on
    /// the Promise, not duplicated in the continuation record.
    pub reactions: Vec<ContinuationId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeObject {
    pub key: NativeKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlainObject {
    pub props: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    pub elements: Vec<Value>,
}

/// A closure. `env` is the lexical environment where the function value was
/// created; `None` is used by direct top-level calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionObject {
    pub func_id: u32,
    pub env: Option<ObjectId>,
}

/// One invocation environment. Bindings are addressed by compile-time IDs, so
/// shadowed names and closures share mutable cells without name lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentObject {
    pub parent: Option<ObjectId>,
    pub bindings: BTreeMap<BindingId, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Object {
    Promise(PromiseObject),
    Native(NativeObject),
    Plain(PlainObject),
    Array(ArrayObject),
    Function(FunctionObject),
    Environment(EnvironmentObject),
}

impl Object {
    pub fn as_promise(&self) -> Option<&PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_promise_mut(&mut self) -> Option<&mut PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_function(&self) -> Option<&FunctionObject> {
        match self {
            Self::Function(function) => Some(function),
            _ => None,
        }
    }

    pub fn as_environment(&self) -> Option<&EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }
}
