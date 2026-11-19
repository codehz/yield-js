//! VM builtins: `Promise` and `queueMicrotask`.
//!
//! `AggregateError` is not a heap object in this slice. `Promise.any` rejects
//! with the string `AggregateError: All promises were rejected` when every
//! input rejects. A later Error-object slice can replace that value.
//!
//! `queueMicrotask` enqueues a VM microtask. It does not call the host, so
//! `Host::call` cannot re-enter the VM from this path. A throw inside that
//! callback has no result promise and is dropped: unhandled rejections are not
//! tracked here.

use crate::error::VmError;
use crate::heap::{
    BuiltinMethod, BuiltinStatic, CombineKind, CombineState, Object, PromiseState, ReactionJob,
    Resolution,
};
use crate::host::Host;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BuiltinCacheKey {
    Top(u32),
    Static { builtin: u32, member: String },
    Method { receiver: ObjectId, name: String },
}

pub(crate) enum BuiltinCall {
    Constructor,
    QueueMicrotask,
    Static(String),
    Method { receiver: ObjectId, name: String },
    Resolution { id: ObjectId },
}

enum Dispatch {
    Script { func_id: u32, env: Option<ObjectId> },
    Builtin(BuiltinCall),
}

const PROMISE_STATICS: [&str; 6] = ["resolve", "reject", "all", "allSettled", "race", "any"];

pub(crate) fn type_error(message: &str) -> Value {
    Value::String(format!("TypeError: {message}"))
}

impl Vm {
    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        let Value::Object(id) = value else {
            return false;
        };
        matches!(
            self.heap.get(*id),
            Ok(Object::Function(_)
                | Object::BuiltinMethod(_)
                | Object::BuiltinStatic(_)
                | Object::Resolution(_))
        )
    }

    pub(crate) fn ensure_top(&mut self, builtin: u32) -> Result<ObjectId, VmError> {
        self.ensure_static(builtin, None)
    }

    pub(crate) fn ensure_static(
        &mut self,
        builtin: u32,
        member: Option<&str>,
    ) -> Result<ObjectId, VmError> {
        let key = match member {
            Some(member) => BuiltinCacheKey::Static {
                builtin,
                member: member.to_owned(),
            },
            None => BuiltinCacheKey::Top(builtin),
        };
        if let Some(id) = self.builtins.get(&key).copied() {
            return Ok(id);
        }
        let name = crate::bytecode::BUILTINS
            .get(builtin as usize)
            .copied()
            .ok_or_else(|| VmError::InvalidState(format!("builtin {builtin} does not exist")))?
            .to_owned();
        let id = self.heap.alloc(Object::BuiltinStatic(BuiltinStatic {
            name,
            member: member.map(str::to_owned),
        }));
        self.builtins.insert(key, id);
        Ok(id)
    }

    pub(crate) fn ensure_method(
        &mut self,
        receiver: ObjectId,
        name: String,
    ) -> Result<ObjectId, VmError> {
        let key = BuiltinCacheKey::Method {
            receiver,
            name: name.clone(),
        };
        if let Some(id) = self.builtins.get(&key).copied() {
            return Ok(id);
        }
        let id = self
            .heap
            .alloc(Object::BuiltinMethod(BuiltinMethod { receiver, name }));
        self.builtins.insert(key, id);
        Ok(id)
    }

    /// `Err` is a script TypeError, not a broken VM.
    pub(crate) fn known_static_member(builtin: u32, name: &str) -> Result<(), Value> {
        if builtin == 0 && PROMISE_STATICS.contains(&name) {
            Ok(())
        } else {
            Err(type_error(&format!(
                "{}.{} is not a function",
                crate::bytecode::BUILTINS
                    .get(builtin as usize)
                    .unwrap_or(&"builtin"),
                name
            )))
        }
    }

    pub(crate) fn invoke(
        &mut self,
        host: &mut dyn Host,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => self.call_function(host, func_id, env, args),
            Some(Dispatch::Builtin(call)) => self.call_builtin(host, call, args),
            None => Ok(Invocation::Thrown(type_error("value is not a function"))),
        }
    }

    fn classify(&self, value: &Value) -> Result<Option<Dispatch>, VmError> {
        let Value::Object(id) = value else {
            return Ok(None);
        };
        if let Some(function) = self.heap.function(*id) {
            return Ok(Some(Dispatch::Script {
                func_id: function.func_id,
                env: function.env,
            }));
        }
        Ok(match self.heap.get(*id)? {
            Object::BuiltinMethod(method) => Some(Dispatch::Builtin(BuiltinCall::Method {
                receiver: method.receiver,
                name: method.name.clone(),
            })),
            Object::BuiltinStatic(function) => static_call(function).ok().map(Dispatch::Builtin),
            Object::Resolution(_) => Some(Dispatch::Builtin(BuiltinCall::Resolution { id: *id })),
            _ => None,
        })
    }
    pub(crate) fn call_builtin(
        &mut self,
        host: &mut dyn Host,
        call: BuiltinCall,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match call {
            BuiltinCall::Constructor => self.construct_promise(host, args),
            BuiltinCall::QueueMicrotask => self.queue_microtask(args),
            BuiltinCall::Static(name) => self.call_promise_static(host, &name, args),
            BuiltinCall::Method { receiver, name } => {
                self.call_promise_method(receiver, &name, args)
            }
            BuiltinCall::Resolution { id } => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                self.capability_call(host, id, value)?;
                Ok(Invocation::Value(Value::Undefined))
            }
        }
    }

    fn construct_promise(
        &mut self,
        host: &mut dyn Host,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let executor = args.first().cloned().unwrap_or(Value::Undefined);
        if !self.is_callable(&executor) {
            return Ok(Invocation::Thrown(type_error(
                "Promise executor is not a function",
            )));
        }
        let promise = self.heap.alloc_promise();
        let (resolve_id, reject_id) = self.alloc_capability_pair(promise)?;
        let invocation = self.invoke(
            host,
            &executor,
            vec![Value::Object(resolve_id), Value::Object(reject_id)],
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, reject_id, error)?;
        }
        Ok(Invocation::Value(Value::Object(promise)))
    }

    fn queue_microtask(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let function = args.first().cloned().unwrap_or(Value::Undefined);
        if !self.is_callable(&function) {
            return Ok(Invocation::Thrown(type_error(
                "queueMicrotask argument is not a function",
            )));
        }
        self.microtasks.push_back(Microtask::Call {
            function,
            args: Vec::new(),
        });
        Ok(Invocation::Value(Value::Undefined))
    }

    fn call_promise_static(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match name {
            "resolve" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = self.heap.alloc_promise();
                self.resolve_promise(host, promise, value)?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "reject" => {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = self.heap.alloc_promise();
                self.settle(promise, Settlement::Rejected(reason))?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "all" => self.combine(host, CombineKind::All, args),
            "allSettled" => self.combine(host, CombineKind::AllSettled, args),
            "race" => self.combine(host, CombineKind::Race, args),
            "any" => self.combine(host, CombineKind::Any, args),
            _ => Ok(Invocation::Thrown(type_error(&format!(
                "Promise.{name} is not a function"
            )))),
        }
    }

    fn call_promise_method(
        &mut self,
        receiver: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        if self.heap.promise(receiver).is_err() {
            return Ok(Invocation::Thrown(type_error(
                "Promise method called on a non-promise",
            )));
        }
        match name {
            "then" => self.promise_then(receiver, args),
            "catch" => {
                let rejected = args.first().cloned();
                self.promise_then(
                    receiver,
                    vec![Value::Undefined, rejected.unwrap_or(Value::Undefined)],
                )
            }
            "finally" => self.promise_finally(receiver, args),
            _ => Ok(Invocation::Thrown(type_error(&format!(
                "Promise.{name} is not a function"
            )))),
        }
    }

    fn promise_then(
        &mut self,
        receiver: ObjectId,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let on_fulfilled = self.handler_arg(args.first());
        let on_rejected = self.handler_arg(args.get(1));
        let result = self.heap.alloc_promise();
        self.react(
            receiver,
            ReactionJob::Script {
                result,
                on_fulfilled,
                on_rejected,
            },
        )?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn promise_finally(
        &mut self,
        receiver: ObjectId,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let on_finally = args.first().cloned().unwrap_or(Value::Undefined);
        let result = self.heap.alloc_promise();
        self.react(receiver, ReactionJob::Finalize { result, on_finally })?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn handler_arg(&self, value: Option<&Value>) -> Option<Value> {
        value.filter(|value| self.is_callable(value)).cloned()
    }

    fn combine(
        &mut self,
        host: &mut dyn Host,
        kind: CombineKind,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        let Some(elements) = self.array_elements(&iterable)? else {
            return Ok(Invocation::Thrown(type_error(
                "Promise combinator iterable is not an array",
            )));
        };
        let result = self.heap.alloc_promise();
        if elements.is_empty() {
            match kind {
                CombineKind::All | CombineKind::AllSettled => {
                    let array = self.heap.alloc_array(Vec::new());
                    self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                }
                CombineKind::Race => {}
                CombineKind::Any => {
                    self.settle(result, Settlement::Rejected(aggregate_error()))?;
                }
            }
            return Ok(Invocation::Value(Value::Object(result)));
        }
        let count = elements.len();
        self.heap.combines.insert(
            result,
            CombineState {
                kind,
                count,
                remaining: count,
                results: vec![Value::Undefined; count],
                states: if kind == CombineKind::AllSettled {
                    vec![PromiseState::Pending; count]
                } else {
                    Vec::new()
                },
                first: None,
                elements: Vec::with_capacity(count),
            },
        );
        for (index, element) in elements.into_iter().enumerate() {
            let slot = self.heap.alloc_promise();
            self.heap
                .combines
                .get_mut(&result)
                .expect("combine state inserted above")
                .elements
                .push(slot);
            self.react(
                slot,
                ReactionJob::Combine {
                    result,
                    index,
                    kind,
                },
            )?;
            self.resolve_promise(host, slot, element)?;
        }
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn array_elements(&self, value: &Value) -> Result<Option<Vec<Value>>, VmError> {
        let Value::Object(id) = value else {
            return Ok(None);
        };
        Ok(match self.heap.get(*id)? {
            Object::Array(array) => Some(array.elements.clone()),
            _ => None,
        })
    }

    pub(crate) fn alloc_capability_pair(
        &mut self,
        promise: ObjectId,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        let resolve_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: 0,
            resolve: true,
            called: false,
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
            called: false,
        }));
        match self.heap.get_mut(resolve_id)? {
            Object::Resolution(resolution) => resolution.pair = reject_id,
            _ => {
                return Err(VmError::InvalidState(
                    "capability allocation did not produce a Resolution".to_owned(),
                ));
            }
        }
        self.heap.resolutions.insert(resolve_id);
        self.heap.resolutions.insert(reject_id);
        Ok((resolve_id, reject_id))
    }

    pub(crate) fn capability_call(
        &mut self,
        host: &mut dyn Host,
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
            self.resolve_promise(host, promise, value)
        } else {
            self.reject_if_open(promise, value)
        }
    }

    fn resolution_called(&self, id: ObjectId) -> Result<bool, VmError> {
        match self.heap.get(id)? {
            Object::Resolution(resolution) => Ok(resolution.called),
            _ => Err(VmError::InvalidState(format!(
                "object {id} is not a resolution function"
            ))),
        }
    }

    fn mark_resolution_called(&mut self, id: ObjectId) -> Result<(), VmError> {
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
        host: &mut dyn Host,
        handler: Option<Value>,
        argument: Value,
        result: ObjectId,
        rejected: bool,
    ) -> Result<(), VmError> {
        let handler = handler.filter(|value| self.is_callable(value));
        let Some(handler) = handler else {
            if rejected {
                return self.settle(result, Settlement::Rejected(argument));
            }
            return self.resolve_promise(host, result, argument);
        };
        match self.invoke(host, &handler, vec![argument])? {
            Invocation::Thrown(error) => self.settle(result, Settlement::Rejected(error)),
            Invocation::Value(value) => self.resolve_promise(host, result, value),
        }
    }

    pub(crate) fn run_queue_call(
        &mut self,
        host: &mut dyn Host,
        function: Value,
        args: Vec<Value>,
    ) -> Result<(), VmError> {
        let _ = self.invoke(host, &function, args)?;
        Ok(())
    }

    pub(crate) fn run_combine(
        &mut self,
        result: ObjectId,
        index: usize,
        kind: CombineKind,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let Some(mut state) = self.heap.combines.get(&result).cloned() else {
            return Ok(());
        };
        if index >= state.count {
            return Err(VmError::InvalidState(format!(
                "combine reaction index {index} is outside {}",
                state.count
            )));
        }
        state.remaining = state.remaining.saturating_sub(1);
        let pending = matches!(self.heap.promise(result)?.state, PromiseState::Pending);
        match kind {
            CombineKind::Race => {
                self.heap.combines.remove(&result);
                if pending {
                    self.settle(result, settlement)?;
                }
                return Ok(());
            }
            CombineKind::All => match settlement {
                Settlement::Rejected(reason) => {
                    self.heap.combines.remove(&result);
                    if pending {
                        self.settle(result, Settlement::Rejected(reason))?;
                    }
                    return Ok(());
                }
                Settlement::Fulfilled(value) => {
                    state.results[index] = value;
                    if state.remaining == 0 && pending {
                        let values = state.results.clone();
                        self.heap.combines.remove(&result);
                        let array = self.heap.alloc_array(values);
                        self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                        return Ok(());
                    }
                }
            },
            CombineKind::AllSettled => {
                state.states[index] = settlement.to_state();
                state.results[index] = settlement_value(&settlement);
                if state.remaining == 0 && pending {
                    let records = state
                        .states
                        .iter()
                        .map(record_from_state)
                        .collect::<Vec<_>>();
                    self.heap.combines.remove(&result);
                    let mut objects = Vec::with_capacity(records.len());
                    for (status, payload) in records {
                        objects.push(self.settlement_record(status, payload)?);
                    }
                    let array = self.heap.alloc_array(objects);
                    self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                    return Ok(());
                }
            }
            CombineKind::Any => match settlement {
                Settlement::Fulfilled(value) => {
                    self.heap.combines.remove(&result);
                    if pending {
                        self.settle(result, Settlement::Fulfilled(value))?;
                    }
                    return Ok(());
                }
                Settlement::Rejected(reason) => {
                    if state.first.is_none() {
                        state.first = Some(reason);
                    }
                    if state.remaining == 0 && pending {
                        self.heap.combines.remove(&result);
                        self.settle(result, Settlement::Rejected(aggregate_error()))?;
                        return Ok(());
                    }
                }
            },
        }
        self.heap.combines.insert(result, state);
        Ok(())
    }

    fn settlement_record(
        &mut self,
        status: &str,
        payload: (&str, Value),
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        let object = Value::Object(id);
        self.heap.set_member(
            &object,
            &Value::String("status".to_owned()),
            Value::String(status.to_owned()),
        )?;
        self.heap
            .set_member(&object, &Value::String(payload.0.to_owned()), payload.1)?;
        Ok(object)
    }
}

fn static_call(function: &BuiltinStatic) -> Result<BuiltinCall, Value> {
    match (function.name.as_str(), function.member.as_deref()) {
        ("Promise", None) => Ok(BuiltinCall::Constructor),
        ("queueMicrotask", None) => Ok(BuiltinCall::QueueMicrotask),
        ("Promise", Some(member)) if PROMISE_STATICS.contains(&member) => {
            Ok(BuiltinCall::Static(member.to_owned()))
        }
        _ => Err(type_error("value is not a function")),
    }
}

fn aggregate_error() -> Value {
    Value::String("AggregateError: All promises were rejected".to_owned())
}

fn settlement_value(settlement: &Settlement) -> Value {
    match settlement {
        Settlement::Fulfilled(value) | Settlement::Rejected(value) => value.clone(),
    }
}

fn record_from_state(state: &PromiseState) -> (&'static str, (&'static str, Value)) {
    match state {
        PromiseState::Fulfilled(value) => ("fulfilled", ("value", value.clone())),
        PromiseState::Rejected(value) => ("rejected", ("reason", value.clone())),
        PromiseState::Pending => ("pending", ("value", Value::Undefined)),
    }
}
