use std::cell::RefCell;
use std::fmt;

use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{Attributes, EventState, EventTargetState, Object, PlainObject, Property};
use crate::intrinsic::{ClassArgs, ClassDef};
use crate::intrinsics::{IntrinsicClass, TypeError};
use crate::runtime::Invocation;
use crate::value::{JsBigInt, JsString, ObjectId, PropertyKey, SymbolId, Value, ValueRef};
use crate::vm::VmCore;

use super::schema::*;
use super::services::Services;

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

    fn rooted_handle(value: Value, epoch: u32, call: u64, roots: &RefCell<HandleRoots>) -> Self {
        roots.borrow_mut().retain(&value);
        Self::handle(value, epoch, call)
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

    /// Initialize an own ordinary object field on a newly allocated instance.
    /// This does not invoke inherited setters or replace an existing field.
    pub fn init_object_field(&self, cx: &mut CallCx<'_>, name: &str) -> JsResult<()> {
        let key = PropertyKey::from(name);
        let object = cx
            .vm
            .heap
            .get(self.id)
            .map_err(|error| JsError::fault(error.to_string()))?;
        if object.own_property(key.borrowed()).is_some() {
            return Err(JsError::fault(format!(
                "host instance already has field {name}"
            )));
        }
        let value = cx.vm.heap.alloc_plain();
        cx.vm
            .heap
            .define_data(self.id, key, Value::object(value), Attributes::ORDINARY)
            .map_err(|error| JsError::fault(error.to_string()))
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

    /// Remove a host instance from an unordered managed list.
    pub fn list_remove_instance<U: HostClass>(
        &self,
        cx: &mut CallCx<'_>,
        index: usize,
        value: Instance<U>,
    ) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        let instance = cx
            .vm
            .host_instances
            .get_mut(&self.id)
            .ok_or_else(|| JsError::fault("value is not a host instance"))?;
        let Some(crate::runtime::HostSlot::JsList(values)) = instance.slots.get_mut(index) else {
            return Err(JsError::fault("slot is not a managed list"));
        };
        if let Some(position) = values
            .iter()
            .position(|item| *item == Value::object(value.id))
        {
            values.swap_remove(position);
        }
        Ok(())
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

/// Runtime-only input to a typed module trampoline.
#[doc(hidden)]
pub struct ModuleDispatch<'a> {
    call: CallCx<'a>,
    owner: NativeTarget,
}

impl<'a> ModuleDispatch<'a> {
    pub(crate) fn new(call: CallCx<'a>, owner: NativeTarget) -> Self {
        Self { call, owner }
    }
}

#[doc(hidden)]
pub fn with_module<'a, M: HostModule>(
    dispatch: ModuleDispatch<'a>,
    run: impl FnOnce(&mut ModuleCx<'a, M>) -> JsResult<HostStep>,
) -> (CallCx<'a>, JsResult<HostStep>) {
    let ModuleDispatch { call, owner } = dispatch;
    let NativeTarget::Export(export) = owner else {
        return (
            call,
            Err(JsError::fault("typed callback is not a module export")),
        );
    };
    let module = export.module as usize;
    if call.bindings.state_type(module) != Some(std::any::TypeId::of::<M>())
        || !call
            .vm
            .module_states
            .get(module)
            .is_some_and(|state| state.payload.is::<M>())
    {
        return (
            call,
            Err(JsError::fault("module state payload type mismatch")),
        );
    }
    // SAFETY: the module declaration above was checked to belong to M.
    let services = unsafe { call.bindings.state_services(module).for_owner::<M>() };
    let mut cx = ModuleCx {
        call,
        module,
        services,
    };
    let result = run(&mut cx);
    (cx.call, result)
}

/// A host export's capability to access its own per-VM module state.
pub struct ModuleCx<'a, M: HostModule> {
    call: CallCx<'a>,
    module: usize,
    services: Services<'a, M>,
}

impl<'a, M: HostModule> std::ops::Deref for ModuleCx<'a, M> {
    type Target = CallCx<'a>;

    fn deref(&self) -> &Self::Target {
        &self.call
    }
}

impl<M: HostModule> std::ops::DerefMut for ModuleCx<'_, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.call
    }
}

impl<'a, M: HostModule> ModuleCx<'a, M> {
    /// The required services declared by this module.
    pub fn services(&self) -> &Services<'a, M> {
        &self.services
    }
}

impl<M: HostModule> ModuleCx<'_, M> {
    pub fn state(&mut self) -> &mut M {
        self.call.vm.module_states[self.module]
            .payload
            .downcast_mut::<M>()
            .expect("module payload validated before dispatch")
    }

    pub fn state_slot(&self, index: usize) -> JsResult<JsValue> {
        match self.call.vm.module_states[self.module].slots.get(index) {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn set_state_slot(&mut self, index: usize, value: JsValue) -> JsResult<()> {
        self.check_value(&value)?;
        match self.call.vm.module_states[self.module].slots.get_mut(index) {
            Some(crate::runtime::HostSlot::Js(slot)) => {
                *slot = value.value;
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

    pub fn state_list_len(&self, index: usize) -> JsResult<usize> {
        match self.call.vm.module_states[self.module].slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_push(&mut self, index: usize, value: JsValue) -> JsResult<()> {
        self.check_value(&value)?;
        match self.call.vm.module_states[self.module].slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value.value);
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
}

/// A class callback with access to the declaring class's linked services.
pub struct ClassCx<'call, 'a, C: HostClass> {
    call: &'call mut CallCx<'a>,
    services: Services<'a, C>,
}

impl<'a, C: HostClass> ClassCx<'_, 'a, C> {
    pub fn services(&self) -> &Services<'a, C> {
        &self.services
    }
}

impl<'a, C: HostClass> std::ops::Deref for ClassCx<'_, 'a, C> {
    type Target = CallCx<'a>;

    fn deref(&self) -> &Self::Target {
        self.call
    }
}

impl<C: HostClass> std::ops::DerefMut for ClassCx<'_, '_, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.call
    }
}

/// Runs a class callback with the declaring class's typed services.
pub fn with_class<'a, C: HostClass, R>(
    call: &mut CallCx<'a>,
    run: impl FnOnce(&mut ClassCx<'_, 'a, C>) -> JsResult<R>,
) -> JsResult<R> {
    let services = call
        .class_owner
        .and_then(|owner| call.bindings.class_callback_services::<C>(owner))
        .ok_or_else(|| JsError::fault("class callback owner mismatch"))?;
    run(&mut ClassCx { call, services })
}

impl TraceRoots for JsValue {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.value.trace(tracer)
    }
}

#[derive(Default)]
pub(crate) struct HandleRoots {
    first: Option<Value>,
    rest: Vec<Value>,
}

impl HandleRoots {
    fn retain(&mut self, value: &Value) {
        if !matches!(value.view(), ValueRef::Object(_) | ValueRef::Symbol(_)) {
            return;
        }
        match &self.first {
            None => self.first = Some(value.clone()),
            Some(first) if self.rest.last().unwrap_or(first) != value => {
                self.rest.push(value.clone())
            }
            Some(_) => {}
        }
    }
}

impl TraceRoots for HandleRoots {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.first.trace(tracer)?;
        self.rest.trace(tracer)
    }
}

impl TraceRoots for RefCell<HandleRoots> {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.borrow().trace(tracer)
    }
}

pub(crate) type InitializeClass<'a> = dyn Fn(
        &mut VmCore,
        &ActiveRoots<'_>,
        &'static ClassDef,
        ObjectId,
        &str,
        ClassArgs<'_>,
    ) -> Result<crate::runtime::Invocation, crate::VmError>
    + 'a;

pub(crate) type InvokeGuest<'a> = dyn Fn(
        &mut VmCore,
        &ActiveRoots<'_>,
        &Value,
        &Value,
        &[Value],
    ) -> Result<Invocation, crate::VmError>
    + 'a;

pub struct CallCx<'a> {
    pub(crate) vm: &'a mut VmCore,
    pub(crate) roots: &'a ActiveRoots<'a>,
    pub(crate) bindings: &'a Bindings,
    pub(crate) class_owner: Option<ClassId>,
    pub(crate) epoch: u32,
    pub(crate) call: u64,
    pub(crate) args: &'a [JsValue],
    pub(crate) this: JsValue,
    pub(crate) construct: bool,
    pub(crate) allocation_prototype: Option<(ClassId, ObjectId)>,
    pub(crate) initialize_class: Option<&'a InitializeClass<'a>>,
    pub(crate) invoke_guest: Option<&'a InvokeGuest<'a>>,
    // Nested guest execution can collect while the callback is active.
    pub(crate) handles: RefCell<HandleRoots>,
}

impl VmCore {
    pub(crate) fn invoke_event_parent(
        &mut self,
        roots: &ActiveRoots<'_>,
        bindings: &Bindings,
        owner: ClassId,
        call: EventParentFn,
        receiver: ObjectId,
    ) -> Result<crate::runtime::Invocation, crate::VmError> {
        use crate::{VmError, runtime::Invocation};

        let call_id = self.next_host_call;
        self.next_host_call = call_id.checked_add(1).unwrap_or(1);
        let epoch = self.epoch;
        let receiver = JsValue::handle(Value::object(receiver), epoch, call_id);
        let mut cx = CallCx {
            vm: self,
            roots,
            bindings,
            class_owner: Some(owner),
            epoch,
            call: call_id,
            args: &[],
            this: receiver.clone(),
            construct: false,
            allocation_prototype: None,
            initialize_class: None,
            invoke_guest: None,
            handles: Default::default(),
        };
        let (value, wrap): (JsValue, fn(Value) -> Invocation) = match call(&mut cx, receiver) {
            Ok(value) => (value, Invocation::Value),
            Err(JsError::Throw(value)) => (value, Invocation::Thrown),
            Err(JsError::Error(error)) => {
                return cx
                    .vm
                    .alloc_intrinsic_message(cx.roots, error.class, Some(error.message))
                    .map(Invocation::Thrown);
            }
            Err(JsError::Fault(fault)) => return Err(VmError::HostFault(fault.0)),
        };
        cx.into_value(value)
            .map(wrap)
            .map_err(|error| VmError::Host(error.0))
    }
}

impl<'a> CallCx<'a> {
    /// The argument handles for this synchronous stage.
    pub fn args(&self) -> &'a [JsValue] {
        self.args
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

    /// Creates a plain Event without consulting mutable guest constructors.
    pub fn event(&mut self, ty: &str, bubbles: bool, cancelable: bool) -> JsResult<JsValue> {
        let event = EventState {
            ty: ty.into(),
            bubbles,
            cancelable,
            time_stamp: self.vm.clock.now(),
            initialized: true,
            ..Default::default()
        };
        let event = self
            .vm
            .alloc_event(None, event)
            .map_err(|error| JsError::Fault(self.fault(error.to_string())))?;
        Ok(self.js_value(event))
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

    /// Call guest code on the current Rust stack without draining microtasks.
    /// A returned Promise is an ordinary value; a thrown value retains its identity.
    pub fn call_sync(
        &mut self,
        callee: JsValue,
        receiver: JsValue,
        args: Vec<JsValue>,
    ) -> JsResult<JsValue> {
        let invoke = self
            .invoke_guest
            .ok_or_else(|| JsError::fault("event parent callbacks cannot call guest code"))?;
        let (callee, receiver, args) = self.unpack_call(callee, receiver, args)?;
        const MAX_SYNC_CALL_DEPTH: u8 = 16;
        if self.vm.sync_call_depth >= MAX_SYNC_CALL_DEPTH {
            let error = self
                .vm
                .make_range_error(self.roots, "host callback nesting limit exceeded")
                .map_err(|error| JsError::fault(error.to_string()))?;
            return Err(self.throw(self.js_value(error)));
        }
        let handles = (&self.handles, self.args);
        let roots = self.roots.with(&handles);
        self.vm.sync_call_depth += 1;
        let result = invoke(self.vm, &roots, &callee, &receiver, &args);
        self.vm.sync_call_depth -= 1;
        match result.map_err(|error| JsError::fault(error.to_string()))? {
            Invocation::Value(value) => Ok(self.js_value(value)),
            Invocation::Thrown(value) => Err(self.throw(self.js_value(value))),
        }
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
        if self.initialize_class.is_none() {
            return Err(JsError::fault("event parent callbacks cannot suspend"));
        }
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
        let (callee, receiver, args) = self.unpack_call(callee, receiver, args)?;
        Ok(Action::Call {
            callee,
            receiver,
            args,
        })
    }

    fn unpack_call(
        &self,
        callee: JsValue,
        receiver: JsValue,
        args: Vec<JsValue>,
    ) -> JsResult<(Value, Value, Vec<Value>)> {
        Ok((
            self.unpack(callee)?,
            self.unpack(receiver)?,
            args.into_iter()
                .map(|value| self.unpack(value))
                .collect::<JsResult<_>>()?,
        ))
    }

    fn suspend(&self, action: Action, saved: Vec<JsValue>, resume: Resume) -> JsResult<HostStep> {
        if self.initialize_class.is_none() {
            return Err(JsError::fault("event parent callbacks cannot suspend"));
        }
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

    pub fn throw(&self, value: JsValue) -> JsError {
        JsError::Throw(value)
    }

    fn type_error(&mut self, message: impl Into<String>) -> JsError {
        let message = message.into();
        match self.vm.type_error(self.roots, &message) {
            Ok(value) => JsError::Throw(self.js_value(value)),
            Err(error) => JsError::Fault(HostFault::new(error.to_string())),
        }
    }

    /// Return the original VM constructor, even if the global binding changed.
    pub fn constructor<C: IntrinsicClass>(&mut self) -> JsResult<JsValue> {
        Ok(self.js_value(C::CLASS.constructor(self.vm)))
    }

    /// Construct an intrinsic with the current call's active stack.
    /// Argument conversion may run guest code and throw its original value.
    pub fn construct<C: IntrinsicClass>(&mut self, args: &[JsValue]) -> JsResult<JsValue> {
        let class = C::CLASS;
        let value = self.initialize_intrinsic(
            class,
            self.vm.builtins.get(class.prototype),
            class.key.owner,
            args,
        )?;
        Ok(self.js_value(value))
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

    fn initialize_intrinsic(
        &mut self,
        class: &'static ClassDef,
        prototype: ObjectId,
        name: &str,
        args: &[JsValue],
    ) -> JsResult<Value> {
        // Intrinsic initialization can invoke guest conversions/getters, even
        // when reached indirectly through alloc_with_super.
        let initialize_class = self.initialize_class.ok_or_else(|| {
            JsError::fault("event parent callbacks cannot initialize intrinsic classes")
        })?;
        for arg in args {
            self.check_value(arg)?;
        }
        let live = (&self.handles, self.args);
        let result = initialize_class(
            self.vm,
            &self.roots.with(&live),
            class,
            prototype,
            name,
            ClassArgs::Handles(args),
        )
        .map_err(|error| JsError::fault(error.to_string()))?;
        match result {
            crate::runtime::Invocation::Value(value) => Ok(value),
            crate::runtime::Invocation::Thrown(value) => Err(self.throw(self.js_value(value))),
        }
    }

    pub fn alloc<T: HostClass>(&mut self, payload: T) -> JsResult<Instance<T>> {
        self.alloc_instance(payload, None)
    }

    /// Pass explicit arguments to the declared standard base constructor.
    /// Conversion may execute guest code and throw the original JavaScript value.
    pub fn alloc_with_super<T: HostClass>(
        &mut self,
        payload: T,
        args: &[JsValue],
    ) -> JsResult<Instance<T>> {
        self.alloc_instance(payload, Some(args))
    }

    fn alloc_instance<T: HostClass>(
        &mut self,
        payload: T,
        args: Option<&[JsValue]>,
    ) -> JsResult<Instance<T>> {
        let class = self
            .bindings
            .class_by_tag(T::TAG)
            .ok_or_else(|| JsError::Fault(self.fault(format!("unregistered class {}", T::TAG))))?;
        let builtin = self.bindings.class_intrinsic_base(class);
        if args.is_some() && builtin.is_none() {
            return Err(JsError::fault(format!(
                "class {} has no standard base",
                T::TAG
            )));
        }
        let record = self.vm.host_classes.get(class).ok_or_else(|| {
            JsError::Fault(self.fault(format!("class {} has no prototype", T::TAG)))
        })?;
        let prototype = self
            .allocation_prototype
            .filter(|(target, _)| *target == record.source)
            .map_or(record.prototype, |(_, prototype)| prototype);
        let payload = Box::new(payload);
        if !self.bindings.class_has_payload_codec(class)
            && !self
                .bindings
                .class_payload_is_empty(class, payload.as_ref())
        {
            return Err(JsError::Fault(HostFault::new(format!(
                "class {} has data but no payload codec",
                T::TAG
            ))));
        }
        let object = if let Some(builtin) = builtin {
            let name = self
                .bindings
                .class_error_stack_name(class, builtin.key.owner);
            let value =
                self.initialize_intrinsic(builtin, prototype, name, args.unwrap_or_default())?;
            let ValueRef::Object(id) = value.view() else {
                unreachable!("class initializer must allocate an object")
            };
            id
        } else if self.bindings.class_is_event_target(class) {
            // EventTarget branding is independent of standard exception bases.
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
        self.handles.get_mut().retain(&Value::object(object));
        Ok(Instance::new(object))
    }

    pub fn this<T: HostClass>(&mut self) -> JsResult<This<T>> {
        let id = Self::instance_id(&self.this);
        Ok(This {
            instance: self.branded_instance(id)?,
        })
    }

    /// Resolve an existing host instance by its VM brand, including subclasses.
    pub fn instance<T: HostClass>(&mut self, value: &JsValue) -> JsResult<Instance<T>> {
        self.check_value(value)?;
        self.branded_instance(Self::instance_id(value))
    }

    /// Resolve a VM-local identity previously obtained from a host instance.
    pub fn instance_by_identity<T: HostClass>(&mut self, id: ObjectId) -> JsResult<Instance<T>> {
        self.branded_instance(Some(id))
    }

    fn instance_id(value: &JsValue) -> Option<ObjectId> {
        match value.value.view() {
            ValueRef::Object(id) => Some(id),
            _ => None,
        }
    }

    fn branded_instance<T: HostClass>(&mut self, id: Option<ObjectId>) -> JsResult<Instance<T>> {
        let Some(id) = id else {
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
        Ok(Instance::new(id))
    }

    pub fn inspect(&self, value: JsValue) -> JsResult<Inspect<'_>> {
        self.check_value(&value)?;
        Ok(Inspect {
            vm: self.vm,
            value,
            epoch: self.epoch,
            call: self.call,
            handles: &self.handles,
        })
    }

    fn encode_operation<O: OperationDriver>(&self, state: &O) -> JsResult<(usize, Vec<u8>)> {
        let declaration = self
            .bindings
            .operation_index::<O>()
            .map_err(JsError::Fault)?;
        let mut bytes = Vec::new();
        // SAFETY: operation_index validated the declared Rust type.
        let services = unsafe { self.bindings.typed_operation_services::<O>(declaration) };
        state
            .encode(&mut bytes, &services)
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
        Err(JsError::new::<TypeError>("expected an AbortSignal"))
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
                self.bindings,
                declaration,
                bytes,
                crate::snapshot::DeliveryTarget::Promise(promise),
                signal,
                |token, bindings, _| {
                    // SAFETY: encode_operation checked O against this declaration.
                    let services = unsafe { bindings.typed_operation_services::<O>(declaration) };
                    Ok(Box::new(TypedRegistration::<O>(
                        state.start(token, &services)?,
                    )))
                },
            )
            .map_err(JsError::Fault)?;
        Ok(self.js_value(Value::object(promise)))
    }

    pub(crate) fn js_value(&self, value: Value) -> JsValue {
        JsValue::rooted_handle(value, self.epoch, self.call, &self.handles)
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

    fn instance_record(&self, id: ObjectId) -> JsResult<&crate::runtime::HostInstance> {
        self.vm
            .host_instances
            .get(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))
    }

    pub(crate) fn slot_value(&self, id: ObjectId, index: usize) -> JsResult<JsValue> {
        let instance = self.instance_record(id)?;
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
        let instance = self.instance_record(id)?;
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
        let instance = self.instance_record(id)?;
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
        let instance = self.instance_record(id)?;
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
    vm: &'a VmCore,
    value: JsValue,
    epoch: u32,
    call: u64,
    handles: &'a RefCell<HandleRoots>,
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
                Property::Data(value) => Some(InspectValue::Data(JsValue::rooted_handle(
                    value,
                    self.epoch,
                    self.call,
                    self.handles,
                ))),
                Property::Accessor { .. } => Some(InspectValue::Accessor),
            },
            None => None,
        }
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

/// Attributes for prototype methods: writable and configurable, not enumerable.
pub(crate) const PROTOTYPE_METHOD_ATTRIBUTES: Attributes = Attributes::METHOD;
