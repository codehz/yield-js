//! Disposal records capture the callable before a binding is initialized. Both
//! stack and lexical cleanup consume the same LIFO state; only the driver differs.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ASYNC_DISPOSE_SYMBOL, DISPOSE_SYMBOL, DisposableResource, DisposeHint, DisposeState, Object,
    ReactionJob,
};
use crate::host::Host;
use crate::resolve::AwaitPrepared;
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

/// An await operand, not a return from cleanup. In particular an undefined
/// operand still requires the caller to schedule an await reaction.
enum DisposeStep {
    Await(Value),
    Done,
}

fn nullish(value: &Value) -> bool {
    matches!(value.view(), ValueRef::Undefined | ValueRef::Null)
}

impl Vm {
    pub(crate) fn create_disposable_resource(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        async_: bool,
    ) -> Result<Result<Option<DisposableResource>, Value>, VmError> {
        if nullish(value) {
            return Ok(Ok(async_.then_some(DisposableResource::AsyncEmpty)));
        }
        if !matches!(value.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error("A disposable resource must be an object")?
            ));
        }
        let roots = &roots.with(value);
        let (symbol, mut hint) = if async_ {
            (ASYNC_DISPOSE_SYMBOL, DisposeHint::Async)
        } else {
            (DISPOSE_SYMBOL, DisposeHint::Sync)
        };
        let method = match self.read_property(host, roots, value, PropertyKeyRef::Symbol(symbol))? {
            Invocation::Thrown(error) => return Ok(Err(error)),
            Invocation::Value(method) if async_ && nullish(&method) => {
                hint = DisposeHint::AsyncFromSync;
                match self.read_property(
                    host,
                    roots,
                    value,
                    PropertyKeyRef::Symbol(DISPOSE_SYMBOL),
                )? {
                    Invocation::Value(method) => method,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                }
            }
            Invocation::Value(method) => method,
        };
        if !self.is_callable(&method) {
            return Ok(Err(
                self.type_error("Resource disposal method is not callable")?
            ));
        }
        Ok(Ok(Some(DisposableResource::Method {
            value: value.clone(),
            method,
            hint,
        })))
    }

    fn disposal(&mut self, state: ObjectId) -> Result<&mut DisposeState, VmError> {
        match self.heap.get_mut(state)? {
            Object::DisposeState(state) => Ok(state),
            _ => Err(VmError::InvalidState("Expected disposal state".into())),
        }
    }

    fn record_disposal_error(&mut self, state: ObjectId, error: Value) -> Result<(), VmError> {
        let error = match self.disposal(state)?.error.take() {
            Some(previous) => self.alloc_suppressed_error(error, previous)?,
            None => error,
        };
        self.disposal(state)?.error = Some(error);
        Ok(())
    }

    fn step_disposal(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
    ) -> Result<DisposeStep, VmError> {
        use DisposableResource::{Adopt, AsyncEmpty, Defer, Method};
        let roots = &roots.with(&state);
        loop {
            let cursor = self.disposal(state)?;
            if cursor.needs_await
                && !cursor.has_awaited
                && matches!(
                    cursor.records.last(),
                    None | Some(
                        Method {
                            hint: DisposeHint::Sync,
                            ..
                        } | Adopt {
                            hint: DisposeHint::Sync,
                            ..
                        } | Defer {
                            hint: DisposeHint::Sync,
                            ..
                        }
                    )
                )
            {
                // An empty async record delays the next synchronous record (or
                // the end), but doesn't count as awaiting an actual method.
                cursor.needs_await = false;
                return Ok(DisposeStep::Await(Value::UNDEFINED));
            }
            let Some(resource) = cursor.records.pop() else {
                return Ok(DisposeStep::Done);
            };
            let (method, receiver, args, hint) = match resource {
                AsyncEmpty => {
                    cursor.needs_await = true;
                    continue;
                }
                Method {
                    value,
                    method,
                    hint,
                } => (method, value, None, hint),
                Adopt {
                    value,
                    method,
                    hint,
                } => (method, Value::UNDEFINED, Some(value), hint),
                Defer { method, hint } => (method, Value::UNDEFINED, None, hint),
            };
            let result = self.invoke(
                host,
                roots,
                &method,
                Arguments::Values(args.as_slice()),
                &receiver,
            )?;
            match (hint, result) {
                (DisposeHint::Async, Invocation::Value(result)) => {
                    self.disposal(state)?.has_awaited = true;
                    return Ok(DisposeStep::Await(result));
                }
                (DisposeHint::AsyncFromSync, result) => {
                    // The wrapper resolves undefined regardless of the sync
                    // result. Even a synchronous throw becomes a rejection.
                    let promise = self.heap.alloc_promise();
                    let settlement = match result {
                        Invocation::Value(_) => Settlement::Fulfilled(Value::UNDEFINED),
                        Invocation::Thrown(error) => Settlement::Rejected(error),
                    };
                    self.settle(promise, settlement)?;
                    self.disposal(state)?.has_awaited = true;
                    return Ok(DisposeStep::Await(Value::object(promise)));
                }
                (_, Invocation::Thrown(error)) => self.record_disposal_error(state, error)?,
                (DisposeHint::Sync, Invocation::Value(_)) => {}
            }
        }
    }

    pub(crate) fn dispose_records(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        records: Vec<DisposableResource>,
        result: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        if records.is_empty() {
            if let Some(result) = result {
                self.settle(result, Settlement::Fulfilled(Value::UNDEFINED))?;
                return Ok(Invocation::Value(Value::object(result)));
            }
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        let state = self.heap.alloc(Object::DisposeState(DisposeState {
            records,
            result,
            ..DisposeState::default()
        }));
        if let Some(result) = result {
            self.heap.promise_mut(result)?.disposal = Some(state);
            self.drive_async_disposal(host, roots, state, result)?;
            return Ok(Invocation::Value(Value::object(result)));
        }
        match self.step_disposal(host, roots, state)? {
            DisposeStep::Done => Ok(match self.disposal(state)?.error.take() {
                Some(error) => Invocation::Thrown(error),
                None => Invocation::Value(Value::UNDEFINED),
            }),
            DisposeStep::Await(_) => Err(VmError::InvalidState(
                "Synchronous disposal attempted to await".into(),
            )),
        }
    }

    fn drive_async_disposal(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
        result: ObjectId,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&state);
        loop {
            let value = match self.step_disposal(host, roots, state)? {
                DisposeStep::Await(value) => value,
                DisposeStep::Done => {
                    let settlement = match self.disposal(state)?.error.take() {
                        Some(error) => Settlement::Rejected(error),
                        None => Settlement::Fulfilled(Value::UNDEFINED),
                    };
                    self.disposal(state)?.result = None;
                    self.heap.promise_mut(result)?.disposal = None;
                    return self.settle(result, settlement);
                }
            };
            let job = ReactionJob::DisposeNext { state, result };
            match self.prepare_await(host, roots, value)? {
                Ok(AwaitPrepared::Promise(promise)) => {
                    self.disposal(state)?.waiting = Some(promise);
                    return self.react(promise, job);
                }
                Ok(AwaitPrepared::Plain(_)) => {
                    return self.enqueue_job(job, Settlement::Fulfilled(Value::UNDEFINED));
                }
                Err(error) => self.record_disposal_error(state, error)?,
            }
        }
    }

    pub(crate) fn resume_disposal(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
        result: ObjectId,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        self.disposal(state)?.waiting = None;
        if let Settlement::Rejected(error) = settlement {
            self.record_disposal_error(state, error.clone())?;
        }
        self.drive_async_disposal(host, roots, state, result)
    }
}
