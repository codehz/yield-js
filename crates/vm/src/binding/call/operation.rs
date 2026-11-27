//! Recoverable host operations. A pre-aborted signal rejects the promise
//! without starting the driver.

use crate::binding::{HostFault, JsError, JsResult, OperationDriver, TypedRegistration};
use crate::intrinsics::TypeError;
use crate::value::{ObjectId, Value, ValueRef};

use super::{CallCx, JsValue};

impl CallCx<'_> {
    pub(super) fn encode_operation<O: OperationDriver>(
        &self,
        state: &O,
    ) -> JsResult<(usize, Vec<u8>)> {
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
}
