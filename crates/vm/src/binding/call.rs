use std::any::Any;
use std::fmt;

use crate::heap::{Attributes, Object, Property};
use crate::value::{JsString, ObjectId, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

use super::schema::*;

// ---------------------------------------------------------------------------
// Values

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Constant,
    Handle { epoch: u32, call: u64 },
}

/// A value crossing the host boundary. Object and symbol identity only make
/// sense within the VM generation and call that produced the handle.
#[derive(Debug, Clone)]
pub struct JsValue {
    pub(crate) value: Value,
    pub(crate) origin: Origin,
}

impl JsValue {
    pub fn undefined() -> Self {
        Self::constant(Value::Undefined)
    }

    pub fn null() -> Self {
        Self::constant(Value::Null)
    }

    pub fn boolean(value: bool) -> Self {
        Self::constant(Value::Bool(value))
    }

    pub fn number(value: f64) -> Self {
        Self::constant(Value::Number(value))
    }

    pub fn string(value: impl Into<JsString>) -> Self {
        Self::constant(Value::String(value.into()))
    }

    fn constant(value: Value) -> Self {
        Self {
            value,
            origin: Origin::Constant,
        }
    }

    pub(crate) fn handle(value: Value, epoch: u32, call: u64) -> Self {
        Self {
            value,
            origin: Origin::Handle { epoch, call },
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self.value, Value::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self.value, Value::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self.value, Value::Object(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(self.value, Value::Symbol(_))
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value {
            Value::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.value {
            Value::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&JsString> {
        match &self.value {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn type_of(&self) -> &'static str {
        self.value.type_of()
    }

    pub fn truthy(&self) -> bool {
        self.value.truthy()
    }

    pub(crate) fn into_constant(self) -> Result<Value, HostFault> {
        match self.origin {
            Origin::Constant => Ok(self.value),
            Origin::Handle { .. } => Err(HostFault::new(
                "a JsValue handle cannot be used outside its call".to_owned(),
            )),
        }
    }
}

impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl fmt::Display for JsValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Value::Undefined => formatter.write_str("undefined"),
            Value::Null => formatter.write_str("null"),
            Value::Bool(value) => write!(formatter, "{value}"),
            Value::Number(value) => write!(formatter, "{value}"),
            Value::String(value) => write!(formatter, "{value:?}"),
            Value::Symbol(id) => write!(formatter, "symbol({id})"),
            Value::Object(id) => write!(formatter, "object({id})"),
        }
    }
}

/// A typed instance allocated by the VM. The marker only exists at compile
/// time; the class tag is checked when reading slots or payload.
pub struct Instance<T: HostClass> {
    id: ObjectId,
    marker: std::marker::PhantomData<T>,
}

impl<T: HostClass> Clone for Instance<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: HostClass> Copy for Instance<T> {}

impl<T: HostClass> Instance<T> {
    pub(crate) fn new(id: ObjectId) -> Self {
        Self {
            id,
            marker: std::marker::PhantomData,
        }
    }

    pub fn value(&self, cx: &CallCx<'_>) -> JsValue {
        cx.js_value(Value::Object(self.id))
    }

    pub fn identity(&self) -> u64 {
        self.id as u64
    }

    pub fn slot(&self, cx: &CallCx<'_>, index: usize) -> JsResult<JsValue> {
        cx.slot_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn set_slot(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.set_slot_value(self.id, index, value)
    }

    pub fn list_len(&self, cx: &CallCx<'_>, index: usize) -> JsResult<usize> {
        cx.list_len_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn list_get(&self, cx: &CallCx<'_>, index: usize, position: usize) -> JsResult<JsValue> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_get_value(self.id, index, position)
    }

    pub fn list_push(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_push_value(self.id, index, value)
    }

    pub fn list_clear(&self, cx: &mut CallCx<'_>, index: usize) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_clear_value(self.id, index)
    }

    pub fn payload<'a>(&self, cx: &'a CallCx<'_>) -> JsResult<&'a T> {
        cx.payload_value::<T>(self.id)
    }
}

pub struct This<T: HostClass> {
    instance: Instance<T>,
}

impl<T: HostClass> This<T> {
    pub fn instance(&self) -> Instance<T> {
        self.instance
    }
}

// ---------------------------------------------------------------------------
// Calls

pub struct CallCx<'a> {
    pub(crate) vm: &'a mut Vm,
    pub(crate) bindings: &'a Bindings,
    pub(crate) epoch: u32,
    pub(crate) call: u64,
    pub(crate) args: Vec<JsValue>,
    pub(crate) this: JsValue,
    pub(crate) construct: bool,
    pub(crate) allocation_prototype: Option<(ClassId, ObjectId)>,
}

impl<'a> CallCx<'a> {
    pub fn args(&self) -> &[JsValue] {
        &self.args
    }

    pub fn arg(&mut self, index: usize) -> JsResult<JsValue> {
        let value = self.args.get(index).cloned();
        value.ok_or_else(|| self.type_error("missing argument"))
    }

    pub fn this_value(&self) -> JsValue {
        self.this.clone()
    }

    pub fn is_construct(&self) -> bool {
        self.construct
    }

    pub fn is_callable(&self, value: &JsValue) -> JsResult<bool> {
        self.check_value(value)?;
        Ok(self.vm.is_callable(&value.value))
    }

    /// Request a script call after this context ends. The resume context has
    /// the original `this` and receives `saved` as its positional arguments.
    pub fn request_call(
        &self,
        callee: JsValue,
        receiver: JsValue,
        args: Vec<JsValue>,
        saved: Vec<JsValue>,
        resume: fn(&mut CallCx<'_>, GuestOutcome) -> JsResult<HostStep>,
    ) -> JsResult<HostStep> {
        Ok(HostStep::Call(GuestCall::new(
            self.epoch,
            self.call,
            self.unpack(callee)?,
            self.unpack(receiver)?,
            args.into_iter()
                .map(|value| self.unpack(value))
                .collect::<JsResult<_>>()?,
            saved
                .into_iter()
                .map(|value| self.unpack(value))
                .collect::<JsResult<_>>()?,
            resume,
        )))
    }

    /// Request synchronous collection after this context ends, without running
    /// microtasks. Resumes with the number of removed objects, the original
    /// `this`, and `saved` as positional arguments. A tracing error aborts the
    /// call without resuming.
    pub fn request_gc(
        &mut self,
        saved: Vec<JsValue>,
        resume: fn(&mut CallCx<'_>, GuestOutcome) -> JsResult<HostStep>,
    ) -> JsResult<HostStep> {
        let saved = saved
            .into_iter()
            .map(|value| self.unpack(value))
            .collect::<JsResult<_>>()?;
        Ok(HostStep::CollectGarbage(GcRequest::new(
            self.epoch, self.call, saved, resume,
        )))
    }

    fn unpack(&self, value: JsValue) -> JsResult<Value> {
        self.check_value(&value)?;
        Ok(value.value)
    }

    pub fn bindings(&self) -> &Bindings {
        self.bindings
    }

    pub fn throw(&self, value: JsValue) -> JsError {
        JsError::Throw(value)
    }

    pub fn throw_string(&self, message: impl Into<String>) -> JsError {
        JsError::Throw(self.js_value(Value::String(message.into().into())))
    }

    pub fn type_error(&mut self, message: impl Into<String>) -> JsError {
        let message = format!("TypeError: {}", message.into());
        match self.vm.type_error(&message) {
            Ok(value) => JsError::Throw(self.js_value(value)),
            Err(error) => JsError::Fault(HostFault::new(error.to_string())),
        }
    }

    pub fn fault(&self, message: impl Into<String>) -> HostFault {
        HostFault::new(message)
    }

    pub fn string<'v>(&mut self, value: &'v JsValue) -> JsResult<&'v JsString> {
        value
            .as_string()
            .ok_or_else(|| self.type_error("expected a string"))
    }

    pub fn number(&mut self, value: &JsValue) -> JsResult<f64> {
        value
            .as_number()
            .ok_or_else(|| self.type_error("expected a number"))
    }

    pub fn same_value(&self, left: &JsValue, right: &JsValue) -> bool {
        crate::ops::same_value(&left.value, &right.value)
    }

    pub fn service<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
        self.bindings.service(id)
    }

    fn state_module<S: crate::binding::HostModuleState>(&self) -> JsResult<usize> {
        self.bindings.state_module(S::MODULE).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!(
                "module state for {:?} is not declared",
                S::MODULE
            )))
        })
    }

    pub fn state<S: crate::binding::HostModuleState>(&mut self) -> JsResult<&mut S> {
        let module = self.state_module::<S>()?;
        let state = self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .ok_or_else(|| JsError::Fault(HostFault::new("module state is missing")))?;
        state
            .payload
            .downcast_mut::<S>()
            .ok_or_else(|| JsError::Fault(HostFault::new("module state payload type mismatch")))
    }

    pub fn state_slot<S: crate::binding::HostModuleState>(
        &self,
        index: usize,
    ) -> JsResult<JsValue> {
        let module = self.state_module::<S>()?;
        match self
            .vm
            .module_states
            .get(module)
            .and_then(Option::as_ref)
            .and_then(|state| state.slots.get(index))
        {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn set_state_slot<S: crate::binding::HostModuleState>(
        &mut self,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let module = self.state_module::<S>()?;
        let value = value.value;
        match self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .and_then(|state| state.slots.get_mut(index))
        {
            Some(crate::runtime::HostSlot::Js(slot)) => {
                *slot = value;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_len<S: crate::binding::HostModuleState>(
        &self,
        index: usize,
    ) -> JsResult<usize> {
        let module = self.state_module::<S>()?;
        match self
            .vm
            .module_states
            .get(module)
            .and_then(Option::as_ref)
            .and_then(|state| state.slots.get(index))
        {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_push<S: crate::binding::HostModuleState>(
        &mut self,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let module = self.state_module::<S>()?;
        let value = value.value;
        match self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .and_then(|state| state.slots.get_mut(index))
        {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn alloc<T: HostClass>(&mut self, payload: T) -> JsResult<Instance<T>> {
        let class = self
            .bindings
            .class_by_tag(T::TAG)
            .ok_or_else(|| JsError::Fault(self.fault(format!("unregistered class {}", T::TAG))))?;
        let record = self.vm.host_classes.get(class).ok_or_else(|| {
            JsError::Fault(self.fault(format!("class {} has no prototype", T::TAG)))
        })?;
        let prototype = self
            .allocation_prototype
            .filter(|(target, _)| *target == record.source)
            .map_or(record.prototype, |(_, prototype)| prototype);
        let payload = Box::new(payload);
        if self.bindings.class_payload_schema(class).is_none()
            && !self
                .bindings
                .class_payload_is_empty(class, payload.as_ref())
        {
            return Err(JsError::Fault(HostFault::new(format!(
                "class {} has data but no payload schema",
                T::TAG
            ))));
        }
        let object = self.vm.heap.alloc_plain_with(Some(prototype));
        let slots = self
            .bindings
            .class_slots(class)
            .iter()
            .map(|slot| match slot.kind {
                SlotKind::Js => crate::runtime::HostSlot::Js(Value::Undefined),
                SlotKind::JsList => crate::runtime::HostSlot::JsList(Vec::new()),
            });
        let slots = slots.collect();
        self.vm.host_instances.insert(
            object,
            crate::runtime::HostInstance {
                class,
                slots,
                payload: Some(payload),
            },
        );
        Ok(Instance::new(object))
    }

    pub fn this<T: HostClass>(&mut self) -> JsResult<This<T>> {
        let Value::Object(id) = self.this.value else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        let Some(class) = self.bindings.class_by_tag(T::TAG) else {
            return Err(JsError::Fault(
                self.fault(format!("unregistered class {}", T::TAG)),
            ));
        };
        let Some(instance) = self.vm.host_instances.get(&id) else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        if !self.bindings.class_is_a(instance.class, class) {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        }
        Ok(This {
            instance: Instance::new(id),
        })
    }

    pub fn inspect(&self, value: JsValue) -> JsResult<Inspect<'_>> {
        self.check_value(&value)?;
        Ok(Inspect {
            vm: self.vm,
            bindings: self.bindings,
            value,
            epoch: self.epoch,
            call: self.call,
        })
    }

    pub fn start_operation<O: OperationDriver>(&mut self, state: O) -> JsResult<JsValue> {
        let declaration = self
            .bindings
            .operation_index::<O>()
            .map_err(JsError::Fault)?;
        let mut bytes = Vec::new();
        state
            .encode(&mut bytes)
            .map_err(|error| JsError::Fault(HostFault(error.0)))?;
        let promise = self.vm.heap.alloc_promise();
        let token = self
            .vm
            .alloc_operation::<O>(declaration, bytes, promise)
            .map_err(JsError::Fault)?;
        match state.start(token, self.bindings) {
            Ok(live) => {
                self.vm.operations.get_mut(&token.id).unwrap().live =
                    Some(Box::new(TypedRegistration::<O>(live)))
            }
            Err(error) => {
                self.vm.operations.remove(&token.id);
                return Err(JsError::Fault(error));
            }
        }
        Ok(self.js_value(Value::Object(promise)))
    }

    pub(crate) fn js_value(&self, value: Value) -> JsValue {
        JsValue::handle(value, self.epoch, self.call)
    }

    /// Translate a declaration-local slot index into the combined layout.
    pub(crate) fn instance_slot<T: HostClass>(&self, index: usize) -> JsResult<usize> {
        let class = self.bindings.class_by_tag(T::TAG).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!("unregistered class {}", T::TAG)))
        })?;
        self.bindings
            .class_slot_base(class)
            .checked_add(index)
            .ok_or_else(|| JsError::Fault(HostFault::new("slot index out of range")))
    }

    pub(crate) fn check_value(&self, value: &JsValue) -> Result<(), JsError> {
        match value.origin {
            Origin::Constant => Ok(()),
            Origin::Handle { epoch, call } if epoch == self.epoch && call == self.call => Ok(()),
            Origin::Handle { .. } => Err(JsError::Fault(HostFault::new(
                "JsValue handle used outside the call that produced it",
            ))),
        }
    }

    pub(crate) fn into_value(self, value: JsValue) -> Result<Value, HostFault> {
        match value.origin {
            Origin::Constant => Ok(value.value),
            Origin::Handle { epoch, call } if epoch == self.epoch && call == self.call => {
                Ok(value.value)
            }
            Origin::Handle { .. } => Err(HostFault::new(
                "JsValue handle used outside the call that produced it",
            )),
        }
    }

    fn instance(&self, id: ObjectId) -> JsResult<&crate::runtime::HostInstance> {
        self.vm
            .host_instances
            .get(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))
    }

    pub(crate) fn slot_value(&self, id: ObjectId, index: usize) -> JsResult<JsValue> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsList(_)) => {
                Err(JsError::Fault(HostFault::new("slot is a JavaScript list")))
            }
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn set_slot_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let value = value.value;
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::Js(slot)) => {
                *slot = value;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsList(_)) => {
                Err(JsError::Fault(HostFault::new("slot is a JavaScript list")))
            }
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_len_value(&self, id: ObjectId, index: usize) -> JsResult<usize> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_get_value(
        &self,
        id: ObjectId,
        index: usize,
        position: usize,
    ) -> JsResult<JsValue> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => values
                .get(position)
                .cloned()
                .map(|value| self.js_value(value))
                .ok_or_else(|| JsError::Fault(HostFault::new("list index out of range"))),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_push_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let value = value.value;
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_clear_value(&mut self, id: ObjectId, index: usize) -> JsResult<()> {
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.clear();
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn payload_value<T: HostClass>(&self, id: ObjectId) -> JsResult<&T> {
        let class = self.bindings.class_by_tag(T::TAG).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!("unregistered class {}", T::TAG)))
        })?;
        let instance = self.instance(id)?;
        if instance.class != class {
            return Err(JsError::Fault(HostFault::new(format!(
                "instance is not a {}",
                T::TAG
            ))));
        }
        instance
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<T>())
            .ok_or_else(|| JsError::Fault(HostFault::new(format!("{} has no payload", T::TAG))))
    }
}

// ---------------------------------------------------------------------------
// Read-only inspection

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    Symbol,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum InspectKey {
    String(JsString),
    Symbol(SymbolId),
}

#[derive(Debug, Clone)]
pub enum InspectValue {
    Data(JsValue),
    Accessor,
}

/// A borrowed, read-only view over one script value. It never invokes getters
/// or proxy traps and cannot write script-visible state.
pub struct Inspect<'a> {
    vm: &'a Vm,
    bindings: &'a Bindings,
    value: JsValue,
    epoch: u32,
    call: u64,
}

impl<'a> Inspect<'a> {
    pub fn kind(&self) -> InspectKind {
        match self.value.value {
            Value::Undefined => InspectKind::Undefined,
            Value::Null => InspectKind::Null,
            Value::Bool(_) => InspectKind::Bool,
            Value::Number(_) => InspectKind::Number,
            Value::String(_) => InspectKind::String,
            Value::Symbol(_) => InspectKind::Symbol,
            Value::Object(_) => InspectKind::Object,
        }
    }

    pub fn value(&self) -> JsValue {
        self.value.clone()
    }

    pub fn identity(&self) -> Option<u64> {
        match self.value.value {
            Value::Object(id) | Value::Symbol(id) => Some(u64::from(id)),
            _ => None,
        }
    }

    pub fn array_length(&self) -> Option<usize> {
        let Value::Object(id) = self.value.value else {
            return None;
        };
        match self.vm.heap.get(id).ok()? {
            Object::Array(array) => Some(array.length),
            _ => None,
        }
    }

    /// Whether this object was created as an Error, independent of its prototype.
    pub fn is_error(&self) -> bool {
        matches!(self.object(), Some(Object::Plain(object)) if object.is_error)
    }

    /// Whether this value is a RegExp object, without following proxies.
    pub fn is_regexp(&self) -> bool {
        matches!(self.object(), Some(Object::RegExp { .. }))
    }

    /// Match from index zero without running methods or reading or writing `lastIndex`.
    pub fn regexp_matches(&self, text: &JsString) -> Option<bool> {
        let Object::RegExp {
            flags, compiled, ..
        } = self.object()?
        else {
            return None;
        };
        let units = crate::regexp::encode_input(text, *flags);
        Some(crate::regexp::find_match(compiled, *flags, &units, 0).is_some())
    }

    /// The stored pattern source and canonical flags, without property access.
    pub fn regexp_source_flags(&self) -> Option<(&JsString, String)> {
        let Object::RegExp { source, flags, .. } = self.object()? else {
            return None;
        };
        Some((source, flags.as_string()))
    }

    fn object(&self) -> Option<&Object> {
        let Value::Object(id) = self.value.value else {
            return None;
        };
        self.vm.heap.get(id).ok()
    }

    pub fn own_keys(&self) -> Vec<InspectKey> {
        let Value::Object(id) = self.value.value else {
            return Vec::new();
        };
        let Ok(object) = self.vm.heap.get(id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for key in object.own_keys(false) {
            let Some(property) = object.own_property(key.borrowed()) else {
                continue;
            };
            if !property.attributes().enumerable {
                continue;
            }
            keys.push(match key {
                PropertyKey::String(key) => InspectKey::String(key),
                PropertyKey::Symbol(key) => InspectKey::Symbol(key),
            });
        }
        keys.sort();
        keys
    }

    pub fn get(&self, key: &InspectKey) -> Option<InspectValue> {
        let Value::Object(id) = self.value.value else {
            return None;
        };
        let key = match key {
            InspectKey::String(key) => PropertyKey::String(key.clone()),
            InspectKey::Symbol(key) => PropertyKey::Symbol(*key),
        };
        let object = self.vm.heap.get(id).ok()?;
        match object.own_property(key.borrowed()) {
            Some(property) => match property.into_owned() {
                Property::Data(value) => Some(InspectValue::Data(JsValue::handle(
                    value, self.epoch, self.call,
                ))),
                Property::Accessor { .. } => Some(InspectValue::Accessor),
            },
            None => None,
        }
    }

    pub fn bindings(&self) -> &'a Bindings {
        self.bindings
    }
}

impl InspectValue {
    pub fn data(&self) -> Option<JsValue> {
        match self {
            Self::Data(value) => Some(value.clone()),
            Self::Accessor => None,
        }
    }
}

/// Marker used by [`Bindings::classes`]; kept public so diagnostics can print
/// the declaration order deterministically.
pub const _CLASS_ORDER_HINT: () = ();

/// Attributes for prototype methods: writable and configurable, not enumerable.
pub(crate) const PROTOTYPE_METHOD_ATTRIBUTES: Attributes = Attributes::METHOD;
