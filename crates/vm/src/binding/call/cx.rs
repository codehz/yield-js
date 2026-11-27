//! Synchronous host call context. Handles are valid only for this generation
//! and call id; nested guest execution can collect while the callback is active.

use std::cell::RefCell;

use crate::binding::{Bindings, ClassId, GuestOutcome, HostClass, HostFault, JsError, JsResult};
use crate::gc::ActiveRoots;
use crate::intrinsic::{ClassArgs, ClassDef};
use crate::intrinsics::IntrinsicClass;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value};
use crate::vm::VmCore;

use super::{HandleRoots, JsValue, Origin};

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
        call: crate::binding::EventParentFn,
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

    pub(super) fn unpack(&self, value: JsValue) -> JsResult<Value> {
        self.check_value(&value)?;
        Ok(value.value)
    }

    pub fn throw(&self, value: JsValue) -> JsError {
        JsError::Throw(value)
    }

    pub(super) fn type_error(&mut self, message: impl Into<String>) -> JsError {
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

    pub fn fault(&self, message: impl Into<String>) -> HostFault {
        HostFault::new(message)
    }

    pub fn string<'v>(&mut self, value: &'v JsValue) -> JsResult<&'v crate::value::JsString> {
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
}
