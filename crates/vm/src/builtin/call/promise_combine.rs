//! Promise combinator state machine: all/any/race/finally.

use super::UNDEFINED_VALUE;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    CombineKind, CombineState, Object, PlainObject, PromiseCallback, PromiseState, Resolution,
};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value};
use crate::vm::VmCore;
impl VmCore {
    pub(crate) fn combine(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        kind: CombineKind,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let root_view = (&args, receiver);
        let roots = &roots.with(&root_view);
        let iterable = args.first().unwrap_or(&UNDEFINED_VALUE);
        let (result, resolve, reject) = match self.new_promise_capability(host, roots, receiver)? {
            Ok(capability) => capability,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let capability_roots = (&result, &resolve, &reject);
        let roots = &roots.with(&capability_roots);
        let reject_result = |vm: &mut Self, host: &Host, roots: &ActiveRoots<'_>, error| {
            vm.call_promise_capability(host, roots, result.clone(), &reject, error)
        };
        let input_resolve =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("resolve"))? {
                Invocation::Value(resolve) if self.is_callable(&resolve) => resolve,
                Invocation::Value(_) => {
                    let error = self.type_error(roots, "Promise.resolve is not callable")?;
                    return reject_result(self, host, roots, error);
                }
                Invocation::Thrown(error) => return reject_result(self, host, roots, error),
            };
        let roots = &roots.with(&input_resolve);
        let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
            Ok(record) => record,
            Err(error) => return reject_result(self, host, roots, error),
        };
        let state = if kind == CombineKind::Race {
            None
        } else {
            let id = self.heap.alloc_plain_with(None);
            self.heap.combines.insert(
                id,
                CombineState {
                    kind,
                    // Keep synchronous element callbacks from finishing before iteration ends.
                    remaining: 1,
                    results: Vec::new(),
                    called: Vec::new(),
                    capability: if kind == CombineKind::Any {
                        reject.clone()
                    } else {
                        resolve.clone()
                    },
                },
            );
            Some(id)
        };
        let iteration_roots = (&iterator, &next, state);
        let roots = &roots.with(&iteration_roots);
        loop {
            let element = match self.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(element)) => element,
                Ok(None) => break,
                Err(error) => return reject_result(self, host, roots, error),
            };
            let resolved = match self.invoke(
                host,
                roots,
                &input_resolve,
                Arguments::Values(&[element]),
                receiver,
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => {
                    let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                    return reject_result(self, host, roots, error);
                }
            };
            let (fulfill_element, reject_element) = if let Some(state) = state {
                let data = self.combine_state_mut(state)?;
                let index = data.results.len();
                data.results.push(Value::UNDEFINED);
                data.called.push(false);
                data.remaining += 1;
                let fulfill_element = if kind == CombineKind::Any {
                    resolve.clone()
                } else {
                    self.alloc_promise_callback(PromiseCallback::CombineElement {
                        state,
                        index,
                        rejected: false,
                    })
                };
                let reject_element = if kind == CombineKind::All {
                    reject.clone()
                } else {
                    self.alloc_promise_callback(PromiseCallback::CombineElement {
                        state,
                        index,
                        rejected: true,
                    })
                };
                (fulfill_element, reject_element)
            } else {
                (resolve.clone(), reject.clone())
            };
            if let Invocation::Thrown(error) = self.invoke_promise_then(
                host,
                roots,
                &resolved,
                vec![fulfill_element, reject_element],
            )? {
                let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                return reject_result(self, host, roots, error);
            }
        }
        if let Some(state) = state
            && let Invocation::Thrown(error) = self.finish_combine_element(host, roots, state)?
        {
            if kind == CombineKind::Any {
                return Ok(Invocation::Thrown(error));
            }
            return reject_result(self, host, roots, error);
        }
        Ok(Invocation::Value(result))
    }

    pub(crate) fn alloc_capability_pair(
        &mut self,
        promise: ObjectId,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        self.alloc_resolution_pair(promise, true)
    }

    pub(crate) fn alloc_resolution_pair(
        &mut self,
        promise: ObjectId,
        track_unreachable: bool,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        let resolve_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: 0,
            resolve: true,
            track_unreachable,
            called: false,
            object: PlainObject::callable(self.heap.function_prototype, 1.0, "".into()),
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
            track_unreachable,
            called: false,
            object: PlainObject::callable(self.heap.function_prototype, 1.0, "".into()),
        }));
        match self.heap.get_mut(resolve_id)? {
            Object::Resolution(resolution) => resolution.pair = reject_id,
            _ => {
                return Err(VmError::InvalidState(
                    "capability allocation did not produce a Resolution".to_owned(),
                ));
            }
        }
        if track_unreachable {
            self.heap.resolutions.insert(resolve_id);
            self.heap.resolutions.insert(reject_id);
        }
        Ok((resolve_id, reject_id))
    }

    pub(crate) fn capability_call(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        value: Value,
    ) -> Result<(), VmError> {
        let (promise, pair, resolve, called) = match self.heap.get(id)? {
            Object::Resolution(resolution) => (
                resolution.promise,
                resolution.pair,
                resolution.resolve,
                resolution.called,
            ),
            _ => {
                return Err(VmError::InvalidState(format!(
                    "object {id} is not a resolution function"
                )));
            }
        };
        if called || self.resolution_called(pair)? {
            return Ok(());
        }
        self.mark_resolution_called(id)?;
        self.mark_resolution_called(pair)?;
        if resolve {
            self.resolve_promise(host, roots, promise, value)
        } else {
            self.reject_if_open(promise, value)
        }
    }

    pub(crate) fn resolution_called(&self, id: ObjectId) -> Result<bool, VmError> {
        match self.heap.get(id)? {
            Object::Resolution(resolution) => Ok(resolution.called),
            _ => Err(VmError::InvalidState(format!(
                "object {id} is not a resolution function"
            ))),
        }
    }

    pub(crate) fn mark_resolution_called(&mut self, id: ObjectId) -> Result<(), VmError> {
        if let Object::Resolution(resolution) = self.heap.get_mut(id)? {
            resolution.called = true;
        }
        self.heap.resolutions.remove(&id);
        Ok(())
    }

    pub(crate) fn is_open(&self, promise: ObjectId) -> Result<bool, VmError> {
        let promise = self.heap.promise(promise)?;
        Ok(!promise.locked && matches!(promise.state, PromiseState::Pending))
    }

    pub(crate) fn reject_if_open(
        &mut self,
        promise: ObjectId,
        reason: Value,
    ) -> Result<(), VmError> {
        if self.is_open(promise)? {
            self.settle(promise, Settlement::Rejected(reason))?;
        }
        Ok(())
    }

    pub(crate) fn run_call_handler(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        handler: &Option<Value>,
        argument: &Value,
        capability: &(Value, Value),
        rejected: bool,
    ) -> Result<(), VmError> {
        let (resolve, reject) = capability;
        let (callback, value) = match handler {
            Some(handler) => {
                match self.invoke(
                    host,
                    roots,
                    handler,
                    Arguments::Values(std::slice::from_ref(argument)),
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => (resolve, value),
                    Invocation::Thrown(error) => (reject, error),
                }
            }
            None if rejected => (reject, argument.clone()),
            None => (resolve, argument.clone()),
        };
        let _ = self.invoke(
            host,
            roots,
            callback,
            Arguments::Values(&[value]),
            &Value::UNDEFINED,
        )?;
        Ok(())
    }

    pub(crate) fn run_queue_call(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        function: &Value,
        args: &[Value],
    ) -> Result<(), VmError> {
        if let Invocation::Thrown(error) = self.invoke(
            host,
            roots,
            function,
            Arguments::Values(args),
            &Value::UNDEFINED,
        )? {
            self.report_exception(host, roots, error)?;
        }
        Ok(())
    }

    fn combine_state_mut(&mut self, id: ObjectId) -> Result<&mut CombineState, VmError> {
        self.heap
            .combines
            .get_mut(&id)
            .ok_or_else(|| VmError::InvalidState(format!("missing Promise combinator state {id}")))
    }

    pub(super) fn call_combine_element(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
        index: usize,
        rejected: bool,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let data = self.combine_state_mut(state)?;
        let called = data.called.get_mut(index).ok_or_else(|| {
            VmError::InvalidState(format!("invalid Promise combinator index {index}"))
        })?;
        if *called {
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        *called = true;
        let all_settled = data.kind == CombineKind::AllSettled;
        let value = if all_settled {
            if rejected {
                self.settlement_record("rejected", ("reason", argument))?
            } else {
                self.settlement_record("fulfilled", ("value", argument))?
            }
        } else {
            argument
        };
        self.combine_state_mut(state)?.results[index] = value;
        match self.finish_combine_element(host, roots, state)? {
            Invocation::Value(_) if all_settled => Ok(Invocation::Value(Value::UNDEFINED)),
            completion => Ok(completion),
        }
    }

    // Release one remaining count, either for an element or for iterator exhaustion.

    fn finish_combine_element(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
    ) -> Result<Invocation, VmError> {
        let data = self.combine_state_mut(state)?;
        data.remaining -= 1;
        if data.remaining != 0 {
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        let kind = data.kind;
        let capability = data.capability.clone();
        // Keep the array lengths intact for previously retained element callbacks.
        let values = data
            .results
            .iter_mut()
            .map(|value| std::mem::replace(value, Value::UNDEFINED))
            .collect();
        let value = if kind == CombineKind::Any {
            self.alloc_aggregate_error(roots, values)?
        } else {
            Value::object(self.heap.alloc_array(values))
        };
        self.invoke(
            host,
            &roots.with(&state),
            &capability,
            Arguments::Values(&[value]),
            &Value::UNDEFINED,
        )
    }

    pub(crate) fn settlement_record(
        &mut self,
        status: &str,
        payload: (&str, Value),
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        self.heap
            .set_own_string(id, "status", Value::string(status.into()))?;
        self.heap.set_own_string(id, payload.0, payload.1)?;
        Ok(Value::object(id))
    }
}
