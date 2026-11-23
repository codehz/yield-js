use std::fmt;

use crate::gc::ActiveRoots;
use crate::heap::{Attributes, EventTargetState, Object, PlainObject, Property};
use crate::value::{JsBigInt, JsString, ObjectId, PropertyKey, SymbolId, Value, ValueRef};
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
        Self::constant(Value::UNDEFINED)
    }

    pub fn null() -> Self {
        Self::constant(Value::NULL)
    }

    pub fn boolean(value: bool) -> Self {
        Self::constant(Value::boolean(value))
    }

    pub fn number(value: f64) -> Self {
        Self::constant(Value::number(value))
    }

    pub fn string(value: impl Into<JsString>) -> Self {
        Self::constant(Value::string(value.into()))
    }

    /// Construct a BigInt from its canonical decimal text.
    pub fn bigint(text: &str) -> JsResult<Self> {
        let value = JsBigInt::from_decimal(text).ok_or_else(|| {
            JsError::Fault(HostFault::new("invalid BigInt decimal text".to_owned()))
        })?;
        Ok(Self::constant(Value::bigint(value)))
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
        matches!(self.value.view(), ValueRef::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self.value.view(), ValueRef::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self.value.view(), ValueRef::Object(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(self.value.view(), ValueRef::Symbol(_))
    }

    pub fn is_bigint(&self) -> bool {
        matches!(self.value.view(), ValueRef::BigInt(_))
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value.view() {
            ValueRef::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.value.view() {
            ValueRef::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&JsString> {
        match self.value.view() {
            ValueRef::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bigint(&self) -> Option<&JsBigInt> {
        match self.value.view() {
            ValueRef::BigInt(value) => Some(value),
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
        match self.value.view() {
            ValueRef::Undefined => formatter.write_str("undefined"),
            ValueRef::Null => formatter.write_str("null"),
            ValueRef::Bool(value) => write!(formatter, "{value}"),
            ValueRef::Number(value) => write!(formatter, "{value}"),
            ValueRef::String(value) => write!(formatter, "{value:?}"),
            ValueRef::BigInt(value) => write!(formatter, "{value}n"),
            ValueRef::Symbol(id) => write!(formatter, "symbol({id})"),
            ValueRef::Object(id) => write!(formatter, "object({id})"),
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
        cx.js_value(Value::object(self.id))
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
    pub(crate) roots: &'a ActiveRoots<'a>,
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

    /// Creates a CustomEvent without consulting mutable guest constructors.
    pub fn custom_event(
        &mut self,
        ty: &str,
        detail: JsValue,
        bubbles: bool,
        cancelable: bool,
    ) -> JsResult<JsValue> {
        let detail = self.unpack(detail)?;
        let event = self
            .vm
            .alloc_custom_event(ty.into(), detail, bubbles, cancelable)
            .map_err(|error| JsError::Fault(self.fault(error.to_string())))?;
        Ok(self.js_value(event))
    }

    /// Dispatch after leaving the native binding stack, then resume with the
    /// cancellation boolean (or the guest exception).
    pub fn request_dispatch_event(
        &self,
        target: JsValue,
        event: JsValue,
        saved: Vec<JsValue>,
        resume: fn(&mut CallCx<'_>, GuestOutcome) -> JsResult<HostStep>,
    ) -> JsResult<HostStep> {
        self.request_call(
            self.event_dispatch_intrinsic(),
            target,
            vec![event],
            saved,
            resume,
        )
    }

    #[doc(hidden)]
    pub fn __linked_dispatch_event(
        &self,
        target: JsValue,
        event: JsValue,
        saved: Vec<JsValue>,
        stage: u32,
    ) -> JsResult<HostStep> {
        self.__linked_call(
            self.event_dispatch_intrinsic(),
            target,
            vec![event],
            saved,
            stage,
        )
    }

    fn event_dispatch_intrinsic(&self) -> JsValue {
        let id = self
            .vm
            .builtins
            .get(crate::builtin::event::TRUSTED_DISPATCH_ROOT);
        self.js_value(Value::object(id))
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
        self.suspend(
            self.call_action(callee, receiver, args)?,
            saved,
            Resume::Inline(resume),
        )
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
        self.suspend(Action::Gc, saved, Resume::Inline(resume))
    }

    #[doc(hidden)]
    pub fn __linked_call(
        &self,
        callee: JsValue,
        receiver: JsValue,
        args: Vec<JsValue>,
        saved: Vec<JsValue>,
        stage: u32,
    ) -> JsResult<HostStep> {
        self.suspend(
            self.call_action(callee, receiver, args)?,
            saved,
            Resume::Linked(stage),
        )
    }

    #[doc(hidden)]
    pub fn __linked_gc(&self, saved: Vec<JsValue>, stage: u32) -> JsResult<HostStep> {
        self.suspend(Action::Gc, saved, Resume::Linked(stage))
    }

    #[doc(hidden)]
    pub fn __linked_effect<O: OperationDriver>(
        &self,
        state: O,
        saved: Vec<JsValue>,
        stage: u32,
    ) -> JsResult<HostStep> {
        let (decl, state) = self.encode_operation(&state)?;
        self.suspend(
            Action::External { decl, state },
            saved,
            Resume::Linked(stage),
        )
    }

    #[doc(hidden)]
    pub fn __spawn(&self, first: HostStep) -> JsResult<HostStep> {
        let request = match first {
            HostStep::Suspend(request) => request,
            HostStep::Return(value) => {
                self.check_value(&value)?;
                SuspendRequest::new(
                    self.epoch,
                    self.call,
                    Action::FinishedSpawn,
                    Vec::new(),
                    Resume::Inline(|_, _| unreachable!("finished task cannot be resumed")),
                )
            }
        };
        Ok(HostStep::Suspend(request.spawning()))
    }

    fn call_action(
        &self,
        callee: JsValue,
        receiver: JsValue,
        args: Vec<JsValue>,
    ) -> JsResult<Action> {
        Ok(Action::Call {
            callee: self.unpack(callee)?,
            receiver: self.unpack(receiver)?,
            args: args
                .into_iter()
                .map(|value| self.unpack(value))
                .collect::<JsResult<_>>()?,
        })
    }

    fn suspend(&self, action: Action, saved: Vec<JsValue>, resume: Resume) -> JsResult<HostStep> {
        let saved = saved
            .into_iter()
            .map(|value| self.unpack(value))
            .collect::<JsResult<_>>()?;
        Ok(HostStep::Suspend(SuspendRequest::new(
            self.epoch, self.call, action, saved, resume,
        )))
    }

    pub fn cancel_task(&mut self, handle: &JsValue) -> JsResult<()> {
        self.check_value(handle)?;
        if let Some(id) = handle.as_number()
            && id > 0.0
            && id < crate::runtime::HOST_TASK_ID_LIMIT as f64
            && id.fract() == 0.0
        {
            self.vm.cancel_host_task(id as u64);
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn __outcome(&self, outcome: GuestOutcome) -> JsResult<JsValue> {
        match outcome {
            GuestOutcome::Returned(value) => Ok(value),
            GuestOutcome::Thrown(value) => Err(self.throw(value)),
        }
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
        JsError::Throw(self.js_value(Value::string(message.into().into())))
    }

    pub fn type_error(&mut self, message: impl Into<String>) -> JsError {
        let message = message.into();
        match self.vm.type_error(self.roots, &message) {
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

    pub fn service<K: ServiceKey>(&self) -> Option<&K::Value> {
        self.bindings.service::<K>()
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
        let object = if self.bindings.class_is_event_target(class) {
            self.vm.heap.alloc(Object::EventTarget {
                object: PlainObject::new(Some(prototype)),
                target: EventTargetState::default(),
            })
        } else {
            self.vm.heap.alloc_plain_with(Some(prototype))
        };
        let slots = self
            .bindings
            .class_slots(class)
            .iter()
            .map(|slot| match slot.kind {
                SlotKind::Js => crate::runtime::HostSlot::Js(Value::UNDEFINED),
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
        let ValueRef::Object(id) = self.this.value.view() else {
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

    fn encode_operation<O: OperationDriver>(&self, state: &O) -> JsResult<(usize, Vec<u8>)> {
        let declaration = self
            .bindings
            .operation_index::<O>()
            .map_err(JsError::Fault)?;
        let mut bytes = Vec::new();
        state
            .encode(&mut bytes)
            .map_err(|error| JsError::Fault(HostFault(error.0)))?;
        Ok((declaration, bytes))
    }

    pub fn promise<O: OperationDriver>(&mut self, state: O) -> JsResult<JsValue> {
        self.start_operation_promise(state, None)
    }

    fn signal_state(
        &self,
        signal: &JsValue,
    ) -> JsResult<(ObjectId, &crate::heap::AbortSignalState)> {
        self.check_value(signal)?;
        if let ValueRef::Object(id) = signal.value.view()
            && let Some(state) = self
                .vm
                .abort_signal_state(id)
                .map_err(|error| JsError::fault(error.to_string()))?
        {
            return Ok((id, state));
        }
        Err(JsError::TypeError("expected an AbortSignal".into()))
    }

    /// Returns the original abort reason, or `None` if the signal is active.
    pub fn abort_reason(&self, signal: &JsValue) -> JsResult<Option<JsValue>> {
        let (_, state) = self.signal_state(signal)?;
        Ok(state.reason.clone().map(|reason| self.js_value(reason)))
    }

    /// Starts an operation whose promise is rejected when `signal` aborts.
    /// If already aborted, the original reason rejects the promise without starting the driver.
    pub fn promise_with_signal<O: OperationDriver>(
        &mut self,
        state: O,
        signal: &JsValue,
    ) -> JsResult<JsValue> {
        let (id, signal) = self.signal_state(signal)?;
        if let Some(reason) = signal.reason.clone() {
            let promise = self.vm.heap.alloc_promise();
            self.vm
                .settle(promise, crate::runtime::Settlement::Rejected(reason))
                .map_err(|error| JsError::fault(error.to_string()))?;
            return Ok(self.js_value(Value::object(promise)));
        }
        self.start_operation_promise(state, Some(id))
    }

    fn start_operation_promise<O: OperationDriver>(
        &mut self,
        state: O,
        signal: Option<ObjectId>,
    ) -> JsResult<JsValue> {
        let (declaration, bytes) = self.encode_operation(&state)?;
        let promise = self.vm.heap.alloc_promise();
        self.vm
            .register_operation(
                declaration,
                bytes,
                crate::snapshot::DeliveryTarget::Promise(promise),
                signal,
                |token, bindings, _| {
                    Ok(Box::new(TypedRegistration::<O>(
                        state.start(token, bindings)?,
                    )))
                },
            )
            .map_err(JsError::Fault)?;
        Ok(self.js_value(Value::object(promise)))
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
    BigInt,
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
        match self.value.value.view() {
            ValueRef::Undefined => InspectKind::Undefined,
            ValueRef::Null => InspectKind::Null,
            ValueRef::Bool(_) => InspectKind::Bool,
            ValueRef::Number(_) => InspectKind::Number,
            ValueRef::String(_) => InspectKind::String,
            ValueRef::BigInt(_) => InspectKind::BigInt,
            ValueRef::Symbol(_) => InspectKind::Symbol,
            ValueRef::Object(_) => InspectKind::Object,
        }
    }

    pub fn value(&self) -> JsValue {
        self.value.clone()
    }

    pub fn bigint(&self) -> Option<&JsBigInt> {
        self.value.as_bigint()
    }

    pub fn identity(&self) -> Option<u64> {
        match self.value.value.view() {
            ValueRef::Object(id) | ValueRef::Symbol(id) => Some(u64::from(id)),
            _ => None,
        }
    }

    pub fn array_length(&self) -> Option<usize> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return None;
        };
        match self.vm.heap.get(id).ok()? {
            Object::Array(array) => Some(array.length),
            _ => None,
        }
    }

    /// Whether this object was created as an Error, independent of its prototype.
    pub fn is_error(&self) -> bool {
        self.object().is_some_and(Object::has_error_brand)
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
        let ValueRef::Object(id) = self.value.value.view() else {
            return None;
        };
        self.vm.heap.get(id).ok()
    }

    pub fn own_keys(&self) -> Vec<InspectKey> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return Vec::new();
        };
        let Ok(object) = self.vm.heap.get(id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for (key, _) in object.enumerable_own_properties() {
            keys.push(match key {
                PropertyKey::String(key) => InspectKey::String(key),
                PropertyKey::Symbol(key) => InspectKey::Symbol(key),
            });
        }
        keys.sort();
        keys
    }

    pub fn get(&self, key: &InspectKey) -> Option<InspectValue> {
        let ValueRef::Object(id) = self.value.value.view() else {
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
