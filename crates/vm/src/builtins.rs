//! VM builtins: promises, microtasks, and object model methods.
//!
//! Script-visible `Error` and `TypeError` are objects. Internal errors remain
//! strings, including the `AggregateError` rejection from `Promise.any`.
//! `Object.prototype` has no constructor because there is no `Object` global.
//!
//! `queueMicrotask` enqueues a VM microtask. It does not call the host, so
//! `Host::call` cannot re-enter the VM from this path. A throw inside that
//! callback has no result promise and is dropped: unhandled rejections are not
//! tracked here.

use std::collections::BTreeMap;

use crate::error::VmError;
use crate::heap::{
    BoundObject, BuiltinStatic, CombineKind, CombineState, Object, PromiseState, ReactionJob,
    Resolution,
};
use crate::host::Host;
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

pub(crate) const ROOT_PROMISE: &str = "promise";
pub(crate) const ROOT_PROMISE_PROTOTYPE: &str = "promise.prototype";
pub(crate) const ROOT_QUEUE_MICROTASK: &str = "queueMicrotask";
pub(crate) const ROOT_OBJECT_PROTOTYPE: &str = "object.prototype";
pub(crate) const ROOT_ERROR_PROTOTYPE: &str = "error.prototype";
pub(crate) const ROOT_TYPEERROR_PROTOTYPE: &str = "typeerror.prototype";
pub(crate) const ROOT_ERROR: &str = "error";
pub(crate) const ROOT_TYPEERROR: &str = "typeerror";

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BuiltinRoots {
    pub promise: ObjectId,
    pub promise_prototype: ObjectId,
    pub queue_microtask: ObjectId,
    pub object_prototype: ObjectId,
    pub error_prototype: ObjectId,
    pub typeerror_prototype: ObjectId,
    pub error: ObjectId,
    pub typeerror: ObjectId,
}

impl BuiltinRoots {
    pub(crate) fn to_map(self) -> BTreeMap<String, ObjectId> {
        BTreeMap::from([
            (ROOT_PROMISE.to_owned(), self.promise),
            (ROOT_PROMISE_PROTOTYPE.to_owned(), self.promise_prototype),
            (ROOT_QUEUE_MICROTASK.to_owned(), self.queue_microtask),
            (ROOT_OBJECT_PROTOTYPE.to_owned(), self.object_prototype),
            (ROOT_ERROR_PROTOTYPE.to_owned(), self.error_prototype),
            (
                ROOT_TYPEERROR_PROTOTYPE.to_owned(),
                self.typeerror_prototype,
            ),
            (ROOT_ERROR.to_owned(), self.error),
            (ROOT_TYPEERROR.to_owned(), self.typeerror),
        ])
    }
}

pub(crate) enum BuiltinCall {
    Constructor,
    ErrorConstructor { name: String },
    FunctionMethod { name: String },
    ObjectMethod { name: String },
    QueueMicrotask,
    Static(String),
    Method { name: String },
    Resolution { id: ObjectId },
}

enum Dispatch<'a> {
    Script { func_id: u32, env: Option<ObjectId> },
    Builtin(BuiltinCall),
    Bound(&'a BoundObject),
}

const PROMISE_STATICS: [&str; 6] = ["resolve", "reject", "all", "allSettled", "race", "any"];
const PROMISE_METHODS: [&str; 3] = ["then", "catch", "finally"];

pub(crate) fn type_error(message: &str) -> Value {
    Value::String(format!("TypeError: {message}"))
}

impl Vm {
    pub(crate) fn init_builtins(&mut self) {
        let object_prototype = self.heap.alloc_plain();
        self.heap.object_prototype = Some(object_prototype);
        let has_own = self.heap.alloc_builtin("Object", Some("hasOwnProperty"));
        self.heap
            .set_own(object_prototype, "hasOwnProperty", Value::Object(has_own))
            .expect("Object.prototype stores methods");

        let prototype = self.heap.alloc_plain();
        let promise = self.heap.alloc_builtin("Promise", None);
        let queue = self.heap.alloc_builtin("queueMicrotask", None);
        for name in PROMISE_STATICS {
            let id = self.heap.alloc_builtin("Promise", Some(name));
            self.heap
                .set_own(promise, name, Value::Object(id))
                .expect("Promise constructor stores statics");
        }
        for name in PROMISE_METHODS {
            let id = self.heap.alloc_builtin("Promise", Some(name));
            self.heap
                .set_own(prototype, name, Value::Object(id))
                .expect("Promise.prototype stores methods");
        }
        self.heap
            .set_own(prototype, "constructor", Value::Object(promise))
            .expect("Promise.prototype.constructor");
        self.heap
            .set_own(promise, "prototype", Value::Object(prototype))
            .expect("Promise.prototype");
        self.heap.promise_prototype = Some(prototype);

        let error_prototype = self.heap.alloc_plain();
        let typeerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        for (id, name) in [
            (error_prototype, "Error"),
            (typeerror_prototype, "TypeError"),
        ] {
            self.heap
                .set_own(id, "name", Value::String(name.to_owned()))
                .expect("Error prototype stores name");
        }
        self.heap
            .set_own(error_prototype, "message", Value::String(String::new()))
            .expect("Error.prototype stores message");
        let error = self.heap.alloc_builtin("Error", None);
        let typeerror = self.heap.alloc_builtin("TypeError", None);
        self.builtins = BuiltinRoots {
            promise,
            promise_prototype: prototype,
            queue_microtask: queue,
            object_prototype,
            error_prototype,
            typeerror_prototype,
            error,
            typeerror,
        };
    }

    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        let Value::Object(id) = value else {
            return false;
        };
        matches!(
            self.heap.get(*id),
            Ok(Object::Function(_)
                | Object::BuiltinStatic(_)
                | Object::Resolution(_)
                | Object::Bound(_))
        )
    }

    pub(crate) fn builtin_id(&self, builtin: u32) -> Result<ObjectId, VmError> {
        match builtin {
            0 => Ok(self.builtins.promise),
            1 => Ok(self.builtins.queue_microtask),
            2 => Ok(self.builtins.error),
            3 => Ok(self.builtins.typeerror),
            _ => Err(VmError::InvalidState(format!(
                "builtin {builtin} does not exist"
            ))),
        }
    }

    pub(crate) fn top_function(&mut self, func_id: u32) -> Result<ObjectId, VmError> {
        if let Some(id) = self.functions.get(&func_id).copied() {
            return Ok(id);
        }
        if self.build.function(func_id).is_none() {
            return Err(VmError::InvalidState(format!(
                "function {func_id} does not exist"
            )));
        }
        let id = self.materialize_function(func_id, None)?;
        self.functions.insert(func_id, id);
        Ok(id)
    }

    pub(crate) fn materialize_function(
        &mut self,
        func_id: u32,
        env: Option<ObjectId>,
    ) -> Result<ObjectId, VmError> {
        let id = self.heap.alloc_function(func_id, env);
        let index = self.function_index(func_id)?;
        let constructable = self.build.functions[index].is_constructable();
        if constructable {
            let prototype = self.heap.alloc_plain();
            self.heap
                .set_own(prototype, "constructor", Value::Object(id))?;
            self.heap
                .set_own(id, "prototype", Value::Object(prototype))?;
        }
        Ok(id)
    }

    pub(crate) fn prototype_write_rejected(&self, object: &Value, key: &str) -> bool {
        key == "prototype" && matches!(object, Value::Object(id) if *id == self.builtins.promise)
    }

    /// `Err` is a script TypeError, not a broken VM.
    pub(crate) fn invoke(
        &mut self,
        host: &mut dyn Host,
        callee: &Value,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => {
                self.call_function(host, func_id, env, args, receiver)
            }
            Some(Dispatch::Builtin(call)) => self.call_builtin(host, call, args, receiver),
            Some(Dispatch::Bound(bound)) => {
                let target = Value::Object(bound.target);
                let bound_this = bound.bound_this.clone();
                let args = bound.args.iter().cloned().chain(args).collect();
                self.invoke(host, &target, args, &bound_this)
            }
            None => Ok(Invocation::Thrown(type_error("value is not a function"))),
        }
    }

    pub(crate) fn construct(
        &mut self,
        host: &mut dyn Host,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => {
                let index = self.function_index(func_id)?;
                let constructable = self.build.functions[index].is_constructable();
                if !constructable {
                    return Ok(Invocation::Thrown(type_error("value is not a constructor")));
                }
                let prototype_value =
                    match self.read_member(host, callee, &Value::String("prototype".to_owned()))? {
                        Invocation::Value(value) => value,
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    };
                let prototype = match prototype_value {
                    Value::Object(id) if self.heap.is_environment(id) != Some(true) => Some(id),
                    _ => None,
                };
                let instance = self.heap.alloc_plain_with(prototype);
                match self.call_function(host, func_id, env, args, &Value::Object(instance))? {
                    Invocation::Thrown(value) => Ok(Invocation::Thrown(value)),
                    Invocation::Value(Value::Object(id)) => {
                        Ok(Invocation::Value(Value::Object(id)))
                    }
                    Invocation::Value(_) => Ok(Invocation::Value(Value::Object(instance))),
                }
            }
            Some(Dispatch::Builtin(BuiltinCall::Constructor)) => self.construct_promise(host, args),
            Some(Dispatch::Builtin(BuiltinCall::ErrorConstructor { name })) => {
                self.construct_error(&name, args)
            }
            _ => Ok(Invocation::Thrown(type_error("value is not a constructor"))),
        }
    }

    fn classify(&self, value: &Value) -> Result<Option<Dispatch<'_>>, VmError> {
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
            Object::BuiltinStatic(function) => static_call(function).ok().map(Dispatch::Builtin),
            Object::Resolution(_) => Some(Dispatch::Builtin(BuiltinCall::Resolution { id: *id })),
            Object::Bound(bound) => Some(Dispatch::Bound(bound)),
            _ => None,
        })
    }

    pub(crate) fn call_builtin(
        &mut self,
        host: &mut dyn Host,
        call: BuiltinCall,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        match call {
            BuiltinCall::Constructor => Ok(Invocation::Thrown(type_error(
                "Promise constructor requires new",
            ))),
            BuiltinCall::ErrorConstructor { name } => self.construct_error(&name, args),
            BuiltinCall::FunctionMethod { name } => {
                self.call_function_method(host, &name, args, receiver)
            }
            BuiltinCall::ObjectMethod { name } => match name.as_str() {
                "hasOwnProperty" => {
                    let id = match *receiver {
                        Value::Object(id) if self.heap.is_environment(id) == Some(false) => id,
                        _ => {
                            return Ok(Invocation::Thrown(type_error(
                                "incompatible Object.prototype receiver",
                            )));
                        }
                    };
                    let key = ops::to_js_string(args.first().unwrap_or(&Value::Undefined));
                    Ok(Invocation::Value(Value::Bool(self.heap.has_own(id, &key)?)))
                }
                _ => Err(VmError::InvalidState(format!(
                    "unknown Object method {name}"
                ))),
            },
            BuiltinCall::QueueMicrotask => self.queue_microtask(args),
            BuiltinCall::Static(name) => {
                if !matches!(receiver, Value::Object(id) if *id == self.builtins.promise) {
                    return Ok(Invocation::Thrown(type_error(
                        "incompatible Promise receiver",
                    )));
                }
                self.call_promise_static(host, &name, args)
            }
            BuiltinCall::Method { name } => {
                let Value::Object(id) = receiver else {
                    return Ok(Invocation::Thrown(type_error(
                        "incompatible Promise receiver",
                    )));
                };
                if self.heap.get(*id)?.as_promise().is_none() {
                    return Ok(Invocation::Thrown(type_error(
                        "incompatible Promise receiver",
                    )));
                }
                self.call_promise_method(*id, &name, args)
            }
            BuiltinCall::Resolution { id } => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                self.capability_call(host, id, value)?;
                Ok(Invocation::Value(Value::Undefined))
            }
        }
    }

    fn call_function_method(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let target = match *receiver {
            Value::Object(id) if self.heap.function(id).is_some() => id,
            _ => {
                return Ok(Invocation::Thrown(type_error(
                    "incompatible Function receiver",
                )));
            }
        };
        let mut args = args.into_iter();
        let this_arg = args.next().unwrap_or(Value::Undefined);
        match name {
            "call" => self.invoke(host, receiver, args.collect(), &this_arg),
            "apply" => {
                let args = match args.next() {
                    None | Some(Value::Undefined | Value::Null) => Vec::new(),
                    Some(Value::Object(id)) if let Object::Array(array) = self.heap.get(id)? => {
                        array.elements.clone()
                    }
                    _ => return Ok(Invocation::Thrown(type_error("createListFromArrayLike"))),
                };
                self.invoke(host, receiver, args, &this_arg)
            }
            "bind" => {
                let id = self.heap.alloc(Object::Bound(BoundObject {
                    target,
                    bound_this: this_arg,
                    args: args.collect(),
                }));
                Ok(Invocation::Value(Value::Object(id)))
            }
            _ => Err(VmError::InvalidState(format!(
                "unknown Function method {name}"
            ))),
        }
    }

    fn construct_error(&mut self, name: &str, args: Vec<Value>) -> Result<Invocation, VmError> {
        let prototype = match name {
            "Error" => self.builtins.error_prototype,
            "TypeError" => self.builtins.typeerror_prototype,
            _ => {
                return Err(VmError::InvalidState(format!(
                    "unknown Error constructor {name}"
                )));
            }
        };
        let id = self.heap.alloc_plain_with(Some(prototype));
        let message = match args.first() {
            Some(value) if *value != Value::Undefined => Some(ops::to_js_string(value)),
            _ => None,
        };
        let stack = format!("{name}: {}", message.as_deref().unwrap_or_default());
        if let Some(message) = message {
            self.heap.set_own(id, "message", Value::String(message))?;
        }
        self.heap.set_own(id, "stack", Value::String(stack))?;
        Ok(Invocation::Value(Value::Object(id)))
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
            &Value::Undefined,
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
        match self.invoke(host, &handler, vec![argument], &Value::Undefined)? {
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
        let _ = self.invoke(host, &function, args, &Value::Undefined)?;
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
        self.heap
            .set_own(id, "status", Value::String(status.to_owned()))?;
        self.heap.set_own(id, payload.0, payload.1)?;
        Ok(Value::Object(id))
    }
}

fn static_call(function: &BuiltinStatic) -> Result<BuiltinCall, Value> {
    match (function.name.as_str(), function.member.as_deref()) {
        ("Promise", None) => Ok(BuiltinCall::Constructor),
        ("queueMicrotask", None) => Ok(BuiltinCall::QueueMicrotask),
        (name @ ("Error" | "TypeError"), None) => Ok(BuiltinCall::ErrorConstructor {
            name: name.to_owned(),
        }),
        ("Function", Some(name @ ("call" | "apply" | "bind"))) => Ok(BuiltinCall::FunctionMethod {
            name: name.to_owned(),
        }),
        ("Object", Some(name @ "hasOwnProperty")) => Ok(BuiltinCall::ObjectMethod {
            name: name.to_owned(),
        }),
        ("Promise", Some(member)) if PROMISE_STATICS.contains(&member) => {
            Ok(BuiltinCall::Static(member.to_owned()))
        }
        ("Promise", Some(member)) if PROMISE_METHODS.contains(&member) => Ok(BuiltinCall::Method {
            name: member.to_owned(),
        }),
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
