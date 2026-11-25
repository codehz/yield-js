//! PromiseResolve and the `await` thenable path.
//!
//! A promise is `locked` once resolution has started via another promise or a
//! fulfilled value. Locking is not settlement: adoption stays pending until
//! the source settles, and that completion still arrives as a microtask.
//! A thenable's `then` is read synchronously, but called in a microtask.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Object, PromiseState};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation, Microtask, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

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

impl VmCore {
    pub(crate) fn resolve_promise(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        result: ObjectId,
        value: Value,
    ) -> Result<(), VmError> {
        if !self.is_open(result)? {
            return Ok(());
        }
        if matches!(value.view(), ValueRef::Object(id) if id == result) {
            let error = self.type_error(roots, "Cannot resolve a promise with itself")?;
            return self.settle(result, Settlement::Rejected(error));
        }
        let input = self.classify_resolution(host, &roots.with(&result), value)?;
        self.resolve_input(result, input)
    }

    /// PromiseResolve(%Promise%, value) can reuse a genuine promise, but only
    /// after observing its constructor. Resolving functions never do this check.
    pub(crate) fn promise_identity(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<Option<ObjectId>, Value>, VmError> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(Ok(None));
        };
        if !matches!(self.heap.get(id)?, Object::Promise(_)) {
            return Ok(Ok(None));
        }
        match self.read_property(host, roots, value, PropertyKeyRef::String("constructor"))? {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Object(constructor) if constructor == self.builtins.get("promise")) => {
                Ok(Ok(Some(id)))
            }
            Invocation::Value(_) => Ok(Ok(None)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn classify_resolution(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        value: Value,
    ) -> Result<ResolutionInput, VmError> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(ResolutionInput::Plain(value));
        };
        let promise = match self.heap.get(id)? {
            Object::Environment(_) | Object::Native(_) => return Ok(ResolutionInput::Plain(value)),
            object => matches!(object, Object::Promise(_)),
        };
        Ok(
            match self.read_property(host, roots, &value, PropertyKeyRef::String("then"))? {
                // The intrinsic then only installs reactions in this subset.
                // Keep the direct adoption link for snapshots.
                Invocation::Value(then) if promise && self.is_intrinsic_promise_then(&then) => {
                    ResolutionInput::Promise(id)
                }
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
        host: &Host,
        roots: &ActiveRoots<'_>,
        result: ObjectId,
        receiver: &Value,
        then: &Value,
    ) -> Result<(), VmError> {
        let (resolve_id, reject_id) = self.alloc_capability_pair(result)?;
        let invocation = self.invoke(
            host,
            &roots.with(&reject_id),
            then,
            Arguments::Values(&[Value::object(resolve_id), Value::object(reject_id)]),
            receiver,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, roots, reject_id, error)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_await(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        input: Value,
    ) -> Result<Result<AwaitPrepared, Value>, VmError> {
        match self.promise_identity(host, roots, &input)? {
            Ok(Some(id)) => return Ok(Ok(AwaitPrepared::Promise(id))),
            Ok(None) => {}
            Err(error) => return Ok(Err(error)),
        }
        match self.classify_resolution(host, roots, input)? {
            ResolutionInput::Plain(value) => Ok(Ok(AwaitPrepared::Plain(value))),
            input => {
                let promise = self.heap.alloc_promise();
                self.resolve_input(promise, input)?;
                Ok(Ok(AwaitPrepared::Promise(promise)))
            }
        }
    }

    pub(crate) fn run_adopt(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        result: ObjectId,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let promise = self.heap.promise_mut(result)?;
        promise.locked = false;
        promise.adopting = None;
        if !matches!(self.heap.promise(result)?.state, PromiseState::Pending) {
            return Ok(());
        }
        match settlement {
            Settlement::Rejected(reason) => {
                self.settle(result, Settlement::Rejected(reason.clone()))
            }
            Settlement::Fulfilled(value) => {
                self.resolve_promise(host, roots, result, value.clone())
            }
        }
    }
}
