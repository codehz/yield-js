//! Object table. Every heap edge is listed by [`Object::write_edges`]; snapshot
//! capture and restore both consume that list instead of re-matching object kinds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::VmError;
use crate::ops::{array_index, property_key};
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

impl PlainObject {
    fn get(&self, key: &str) -> Value {
        self.props.get(key).cloned().unwrap_or(Value::Undefined)
    }

    fn set(&mut self, key: String, value: Value) {
        self.props.insert(key, value);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    pub elements: Vec<Value>,
}

impl ArrayObject {
    fn get(&self, key: &str) -> Value {
        if key == "length" {
            return Value::Number(self.elements.len() as f64);
        }
        array_index(key)
            .and_then(|index| self.elements.get(index).cloned())
            .unwrap_or(Value::Undefined)
    }

    fn set(&mut self, key: &str, value: Value) -> Result<(), VmError> {
        if key == "length" {
            let Value::Number(length) = value else {
                return Err(VmError::InvalidState(
                    "array length must be a number".to_owned(),
                ));
            };
            if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
                return Err(VmError::InvalidState(
                    "array length must be a non-negative integer".to_owned(),
                ));
            }
            self.elements.resize(length as usize, Value::Undefined);
            return Ok(());
        }
        let Some(index) = array_index(key) else {
            return Err(VmError::InvalidState(format!(
                "unsupported array property {key:?}"
            )));
        };
        if self.elements.len() <= index {
            self.elements.resize(index + 1, Value::Undefined);
        }
        self.elements[index] = value;
        Ok(())
    }
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

/// Why an object id must be an environment. The role selects the restore error.
#[derive(Clone, Copy)]
pub(crate) enum EnvRole {
    Closure,
    Parent,
}

/// Owned edge so a walker can drop the source object and then query the table.
#[derive(Clone, Copy)]
pub(crate) enum OwnedEdge {
    Object(ObjectId),
    Environment { id: ObjectId, role: EnvRole },
    Continuation(ContinuationId),
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

    pub fn as_environment_mut(&mut self) -> Option<&mut EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }

    /// Append every outgoing reference. Callers clear `out` only by relying on
    /// this method, which replaces the buffer contents.
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.clear();
        match self {
            Self::Promise(promise) => {
                if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) =
                    &promise.state
                {
                    push_value(out, value);
                }
                for continuation in &promise.reactions {
                    out.push(OwnedEdge::Continuation(*continuation));
                }
            }
            Self::Native(_) => {}
            Self::Plain(plain) => {
                for value in plain.props.values() {
                    push_value(out, value);
                }
            }
            Self::Array(array) => {
                for value in &array.elements {
                    push_value(out, value);
                }
            }
            Self::Function(function) => {
                if let Some(id) = function.env {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Closure,
                    });
                }
            }
            Self::Environment(environment) => {
                if let Some(id) = environment.parent {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Parent,
                    });
                }
                for value in environment.bindings.values() {
                    push_value(out, value);
                }
            }
        }
    }
}

fn push_value(out: &mut Vec<OwnedEdge>, value: &Value) {
    if let Value::Object(id) = value {
        out.push(OwnedEdge::Object(*id));
    }
}

#[derive(Debug)]
pub(crate) struct Heap {
    objects: BTreeMap<ObjectId, Object>,
    next_id: ObjectId,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub(crate) fn alloc(&mut self, object: Object) -> ObjectId {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("object id space exhausted");
        self.objects.insert(id, object);
        id
    }

    pub(crate) fn alloc_promise(&mut self) -> ObjectId {
        self.alloc(Object::Promise(PromiseObject {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        }))
    }

    pub(crate) fn alloc_plain(&mut self) -> ObjectId {
        self.alloc(Object::Plain(PlainObject {
            props: BTreeMap::new(),
        }))
    }

    pub(crate) fn alloc_array(&mut self, elements: Vec<Value>) -> ObjectId {
        self.alloc(Object::Array(ArrayObject { elements }))
    }

    pub(crate) fn alloc_function(&mut self, func_id: u32, env: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Function(FunctionObject { func_id, env }))
    }

    pub(crate) fn alloc_environment(
        &mut self,
        parent: Option<ObjectId>,
        bindings: BTreeMap<BindingId, Value>,
    ) -> ObjectId {
        self.alloc(Object::Environment(EnvironmentObject { parent, bindings }))
    }

    pub(crate) fn insert_restore(&mut self, id: ObjectId, object: Object) -> Result<(), VmError> {
        if self.objects.insert(id, object).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate object id {id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn finish_restore(&mut self) {
        self.next_id = self.objects.keys().next_back().map_or(1, |id| id + 1);
    }

    pub(crate) fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    pub(crate) fn get(&self, id: ObjectId) -> Result<&Object, VmError> {
        self.objects.get(&id).ok_or_else(|| missing_object(id))
    }

    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Result<&mut Object, VmError> {
        self.objects.get_mut(&id).ok_or_else(|| missing_object(id))
    }

    pub(crate) fn cloned(&self, id: ObjectId) -> Result<Object, VmError> {
        self.get(id).cloned()
    }

    /// `None` if missing, otherwise whether the object is an environment.
    pub(crate) fn is_environment(&self, id: ObjectId) -> Option<bool> {
        self.objects
            .get(&id)
            .map(|object| matches!(object, Object::Environment(_)))
    }

    pub(crate) fn promise(&self, id: ObjectId) -> Result<&PromiseObject, VmError> {
        self.get(id)?.as_promise().ok_or_else(|| not_promise(id))
    }

    pub(crate) fn promise_mut(&mut self, id: ObjectId) -> Result<&mut PromiseObject, VmError> {
        self.get_mut(id)?
            .as_promise_mut()
            .ok_or_else(|| not_promise(id))
    }

    pub(crate) fn environment(&self, id: ObjectId) -> Result<&EnvironmentObject, VmError> {
        self.get(id)?
            .as_environment()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn environment_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<&mut EnvironmentObject, VmError> {
        self.get_mut(id)?
            .as_environment_mut()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn function(&self, id: ObjectId) -> Option<&FunctionObject> {
        self.objects.get(&id).and_then(Object::as_function)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ObjectId, &Object)> {
        self.objects.iter().map(|(id, object)| (*id, object))
    }

    pub(crate) fn ids(&self) -> Vec<ObjectId> {
        self.objects.keys().copied().collect()
    }

    pub(crate) fn intern_native(&mut self, key: NativeKey) -> ObjectId {
        if let Some(id) = self.objects.iter().find_map(|(id, object)| match object {
            Object::Native(native) if native.key == key => Some(*id),
            _ => None,
        }) {
            return id;
        }
        self.alloc(Object::Native(NativeObject { key }))
    }

    pub(crate) fn get_member(&self, object: &Value, key: &Value) -> Result<Value, VmError> {
        let key = property_key(key)?;
        let id = object_id(object, "property access on a non-object")?;
        match self.get(id)? {
            Object::Plain(plain) => Ok(plain.get(&key)),
            Object::Array(array) => Ok(array.get(&key)),
            _ => Err(VmError::InvalidState(format!(
                "object {id} does not expose script properties"
            ))),
        }
    }

    pub(crate) fn set_member(
        &mut self,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<(), VmError> {
        let key = property_key(key)?;
        let id = object_id(object, "property write on a non-object")?;
        match self.get_mut(id)? {
            Object::Plain(plain) => {
                plain.set(key, value);
                Ok(())
            }
            Object::Array(array) => array.set(&key, value),
            _ => Err(VmError::InvalidState(format!(
                "object {id} does not expose script properties"
            ))),
        }
    }

    pub(crate) fn get_binding(&self, env: ObjectId, binding: BindingId) -> Result<Value, VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not initialized"
            )));
        };
        self.environment(owner)?
            .bindings
            .get(&binding)
            .cloned()
            .ok_or_else(|| VmError::InvalidState(format!("binding {binding} is not initialized")))
    }

    pub(crate) fn set_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
        value: Value,
    ) -> Result<(), VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not declared"
            )));
        };
        self.environment_mut(owner)?.bindings.insert(binding, value);
        Ok(())
    }

    pub(crate) fn declare_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<(), VmError> {
        self.environment_mut(env)?
            .bindings
            .entry(binding)
            .or_insert(Value::Undefined);
        Ok(())
    }

    fn binding_owner(
        &self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<Option<ObjectId>, VmError> {
        let mut current = Some(env);
        while let Some(id) = current {
            let environment = self.environment(id)?;
            if environment.bindings.contains_key(&binding) {
                return Ok(Some(id));
            }
            current = environment.parent;
        }
        Ok(None)
    }
}

fn object_id(value: &Value, what: &str) -> Result<ObjectId, VmError> {
    match value {
        Value::Object(id) => Ok(*id),
        _ => Err(VmError::InvalidState(what.to_owned())),
    }
}

fn missing_object(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} does not exist"))
}

fn not_promise(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not a Promise"))
}

fn not_environment(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not an environment"))
}
