//! PromiseResolve and the `await` thenable path.
//!
//! A promise is `locked` once resolution has started via another promise or a
//! fulfilled value. Locking is not settlement: adoption stays pending until
//! the source settles, and that completion still arrives as a microtask.
//! Resolving a promise with itself is ignored, matching this slice's contract
//! rather than the spec TypeError.

use crate::error::VmError;
use crate::heap::{Object, PromiseState, ReactionJob};
use crate::host::Host;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

pub(crate) enum AwaitPrepared {
    Promise(ObjectId),
    Plain(Value),
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
        if let Value::Object(id) = value {
            if self.heap.get(id)?.as_promise().is_some() {
                if id == result {
                    return Ok(());
                }
                let source = Settlement::from_state(&self.heap.promise(id)?.state);
                {
                    let promise = self.heap.promise_mut(result)?;
                    promise.locked = true;
                    promise.adopting = Some(id);
                }
                match source {
                    Some(settlement) => {
                        self.microtasks
                            .push_back(Microtask::Adopt { result, settlement });
                    }
                    None => {
                        self.heap
                            .promise_mut(id)?
                            .reactions
                            .push(ReactionJob::Adopt { result });
                    }
                }
                return Ok(());
            }
            if let Some(then_fn) = self.thenable_then(id)? {
                let (resolve_id, reject_id) = self.alloc_capability_pair(result)?;
                let invocation = self.invoke(
                    host,
                    &then_fn,
                    vec![Value::Object(resolve_id), Value::Object(reject_id)],
                    &Value::Object(id),
                )?;
                if let Invocation::Thrown(error) = invocation {
                    self.capability_call(host, reject_id, error)?;
                }
                return Ok(());
            }
        }
        self.heap.promise_mut(result)?.locked = true;
        self.settle(result, Settlement::Fulfilled(value))
    }

    pub(crate) fn thenable_then(&self, id: ObjectId) -> Result<Option<Value>, VmError> {
        if matches!(
            self.heap.get(id)?,
            Object::Environment(_) | Object::Native(_)
        ) {
            return Ok(None);
        }
        let then = self
            .heap
            .get_member(&Value::Object(id), &Value::String("then".to_owned()))?;
        if matches!(then, Value::Undefined) {
            return Ok(None);
        }
        Ok(Some(then).filter(|value| self.is_callable(value)))
    }

    pub(crate) fn is_assimilable(&self, value: &Value) -> Result<bool, VmError> {
        let Value::Object(id) = value else {
            return Ok(false);
        };
        if self.heap.get(*id)?.as_promise().is_some() {
            return Ok(true);
        }
        Ok(self.thenable_then(*id)?.is_some())
    }

    pub(crate) fn prepare_await(
        &mut self,
        host: &mut dyn Host,
        input: Value,
    ) -> Result<AwaitPrepared, VmError> {
        let Value::Object(id) = input else {
            return Ok(AwaitPrepared::Plain(input));
        };
        if self.heap.get(id)?.as_promise().is_some() {
            return Ok(AwaitPrepared::Promise(id));
        }
        if self.thenable_then(id)?.is_some() {
            let promise = self.heap.alloc_promise();
            self.resolve_promise(host, promise, Value::Object(id))?;
            return Ok(AwaitPrepared::Promise(promise));
        }
        Err(VmError::InvalidState(
            "await only supports VM promises and plain values in this PoC".to_owned(),
        ))
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
            Invocation::Value(value) => {
                if self.is_assimilable(&value)? {
                    if !matches!(self.heap.promise(result)?.state, PromiseState::Pending) {
                        return Ok(());
                    }
                    {
                        let promise = self.heap.promise_mut(result)?;
                        promise.locked = true;
                        promise.finally_settlement = Some(settlement.to_state());
                    }
                    let tail = self.heap.alloc_promise();
                    self.heap.promise_mut(result)?.adopting = Some(tail);
                    self.react(tail, ReactionJob::Adopt { result })?;
                    self.resolve_promise(host, tail, value)
                } else {
                    self.apply_saved(host, result, &settlement.to_state())
                }
            }
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
