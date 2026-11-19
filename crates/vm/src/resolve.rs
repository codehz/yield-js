//! PromiseResolve and the `await` thenable path.
//!
//! A promise is `locked` once resolution has started via another promise or a
//! fulfilled value. Locking is not settlement: adoption stays pending until
//! the source settles, and that completion still arrives as a microtask.
//! A thenable's `then` is read synchronously, but called in a microtask.

use crate::error::VmError;
use crate::heap::{Object, PromiseState, ReactionJob};
use crate::host::Host;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value};
use crate::vm::Vm;

pub(crate) enum AwaitPrepared {
    Promise(ObjectId),
    Plain(Value),
}

enum ResolutionInput {
    Promise(ObjectId),
    Plain(Value),
    Thenable { receiver: Value, then: Value },
    Thrown(Value),
}

impl Vm {
    pub(crate) fn resolve_promise(
        &mut self,
        host: &mut dyn Host,
        result: ObjectId,
        value: Value,
    ) -> Result<(), VmError> {
        if !self.is_open(result)? {
            return Ok(());
        }
        if matches!(value, Value::Object(id) if id == result) {
            let error = self.type_error("Cannot resolve a promise with itself")?;
            return self.settle(result, Settlement::Rejected(error));
        }
        let input = self.classify_resolution(host, value)?;
        self.resolve_input(result, input)
    }

    fn classify_resolution(
        &mut self,
        host: &mut dyn Host,
        value: Value,
    ) -> Result<ResolutionInput, VmError> {
        let Value::Object(id) = value else {
            return Ok(ResolutionInput::Plain(value));
        };
        match self.heap.get(id)? {
            Object::Promise(_) => return Ok(ResolutionInput::Promise(id)),
            Object::Environment(_) | Object::Native(_) => return Ok(ResolutionInput::Plain(value)),
            _ => {}
        }
        Ok(
            match self.read_property(host, &value, PropertyKeyRef::String("then"))? {
                Invocation::Value(then) if self.is_callable(&then) => ResolutionInput::Thenable {
                    receiver: value,
                    then,
                },
                Invocation::Value(_) => ResolutionInput::Plain(value),
                Invocation::Thrown(error) => ResolutionInput::Thrown(error),
            },
        )
    }

    fn resolve_input(&mut self, result: ObjectId, input: ResolutionInput) -> Result<(), VmError> {
        match input {
            ResolutionInput::Promise(id) => {
                debug_assert_ne!(id, result);
                let promise = self.heap.promise_mut(result)?;
                promise.locked = true;
                promise.adopting = Some(id);
                self.microtasks
                    .push_back(Microtask::BeginAdopt { result, source: id });
                Ok(())
            }
            ResolutionInput::Thenable { receiver, then } => {
                self.microtasks.push_back(Microtask::Assimilate {
                    result,
                    receiver,
                    then,
                });
                Ok(())
            }
            ResolutionInput::Thrown(error) => self.reject_if_open(result, error),
            ResolutionInput::Plain(value) => {
                self.heap.promise_mut(result)?.locked = true;
                self.settle(result, Settlement::Fulfilled(value))
            }
        }
    }

    pub(crate) fn run_assimilate(
        &mut self,
        host: &mut dyn Host,
        result: ObjectId,
        receiver: Value,
        then: Value,
    ) -> Result<(), VmError> {
        let (resolve_id, reject_id) = self.alloc_capability_pair(result)?;
        let invocation = self.invoke(
            host,
            &then,
            vec![Value::Object(resolve_id), Value::Object(reject_id)],
            &receiver,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, reject_id, error)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_await(
        &mut self,
        host: &mut dyn Host,
        input: Value,
    ) -> Result<AwaitPrepared, VmError> {
        match self.classify_resolution(host, input)? {
            ResolutionInput::Plain(value) => Ok(AwaitPrepared::Plain(value)),
            ResolutionInput::Promise(id) => Ok(AwaitPrepared::Promise(id)),
            input => {
                let promise = self.heap.alloc_promise();
                self.resolve_input(promise, input)?;
                Ok(AwaitPrepared::Promise(promise))
            }
        }
    }

    pub(crate) fn run_finalize(
        &mut self,
        host: &mut dyn Host,
        result: ObjectId,
        on_finally: Value,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        if !self.is_callable(&on_finally) {
            return self.apply_saved(host, result, &settlement.to_state());
        }
        match self.invoke(host, &on_finally, Vec::new(), &Value::Undefined)? {
            Invocation::Thrown(error) => self.reject_if_open(result, error),
            Invocation::Value(value) => match self.classify_resolution(host, value)? {
                ResolutionInput::Plain(_) => self.apply_saved(host, result, &settlement.to_state()),
                ResolutionInput::Thrown(error) => {
                    self.resolve_input(result, ResolutionInput::Thrown(error))
                }
                input => {
                    if !matches!(self.heap.promise(result)?.state, PromiseState::Pending) {
                        return Ok(());
                    }
                    let tail = self.heap.alloc_promise();
                    let promise = self.heap.promise_mut(result)?;
                    promise.locked = true;
                    promise.finally_settlement = Some(settlement.to_state());
                    promise.adopting = Some(tail);
                    self.react(tail, ReactionJob::Adopt { result })?;
                    self.resolve_input(tail, input)
                }
            },
        }
    }

    pub(crate) fn run_adopt(
        &mut self,
        host: &mut dyn Host,
        result: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let saved = {
            let promise = self.heap.promise_mut(result)?;
            promise.locked = false;
            promise.adopting = None;
            promise.finally_settlement.take()
        };
        if !matches!(self.heap.promise(result)?.state, PromiseState::Pending) {
            return Ok(());
        }
        if let Some(saved) = saved {
            return match settlement {
                Settlement::Rejected(reason) => self.settle(result, Settlement::Rejected(reason)),
                Settlement::Fulfilled(_) => self.apply_saved(host, result, &saved),
            };
        }
        match settlement {
            Settlement::Rejected(reason) => self.settle(result, Settlement::Rejected(reason)),
            Settlement::Fulfilled(value) => self.resolve_promise(host, result, value),
        }
    }

    pub(crate) fn apply_saved(
        &mut self,
        host: &mut dyn Host,
        result: ObjectId,
        saved: &PromiseState,
    ) -> Result<(), VmError> {
        let Some(settlement) = Settlement::from_state(saved) else {
            return Ok(());
        };
        if !self.is_open(result)? {
            return Ok(());
        }
        match settlement {
            Settlement::Fulfilled(value) => self.resolve_promise(host, result, value),
            Settlement::Rejected(reason) => self.settle(result, Settlement::Rejected(reason)),
        }
    }
}
