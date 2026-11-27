//! Guest calls, event dispatch, and suspension. Event-parent callbacks cannot
//! suspend or execute guest code.

use crate::binding::{Action, GuestOutcome, HostStep, JsError, JsResult, Resume};
use crate::heap::EventState;
use crate::runtime::Invocation;
use crate::value::Value;

use super::{CallCx, JsValue};

impl CallCx<'_> {
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
    pub fn __linked_effect<O: crate::binding::OperationDriver>(
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
                crate::binding::SuspendRequest::new(
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
        Ok(HostStep::Suspend(crate::binding::SuspendRequest::new(
            self.epoch, self.call, action, saved, resume,
        )))
    }
}
