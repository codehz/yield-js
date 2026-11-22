//! Promise construction, then, species, and capability plumbing.

use crate::builtin::promise::members::THEN;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{CombineKind, Object, PlainObject, PromiseCallback, ReactionJob, SPECIES_SYMBOL};
use crate::host::Host;
use crate::intrinsic::CallableTarget;
use crate::runtime::{Arguments, Invocation, Microtask};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
impl Vm {
    pub(crate) fn construct_promise(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let executor = args.first().cloned().unwrap_or(Value::UNDEFINED);
        if !self.is_callable(&executor) {
            return self.throw_type_error(roots, "Promise executor is not a function");
        }
        let promise = self.heap.alloc_promise();
        if let Some(prototype) = prototype {
            let accepted = self.heap.set_prototype(promise, Some(prototype))?;
            debug_assert!(accepted, "fresh Promise accepts its prototype");
        }
        let (resolve_id, reject_id) = self.alloc_capability_pair(promise)?;
        let root_view = (promise, reject_id);
        let roots = &roots.with(&root_view);
        let capability = [Value::object(resolve_id), Value::object(reject_id)];
        let invocation = self.invoke(
            host,
            roots,
            &executor,
            Arguments::Values(&capability),
            &Value::UNDEFINED,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, roots, reject_id, error)?;
        }
        Ok(Invocation::Value(Value::object(promise)))
    }

    pub(crate) fn queue_microtask(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let function = args.first().cloned().unwrap_or(Value::UNDEFINED);
        if !self.is_callable(&function) {
            return self.throw_type_error(roots, "queueMicrotask argument is not a function");
        }
        self.microtasks.push_back(Microtask::Call {
            function,
            args: Vec::new(),
        });
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    pub(crate) fn is_intrinsic_promise_then(&self, value: &Value) -> bool {
        static THEN_ID: std::sync::LazyLock<crate::intrinsic::IntrinsicId> =
            std::sync::LazyLock::new(|| THEN.link());
        matches!(value.view(), ValueRef::Object(id) if matches!(self.heap.get(id),
            Ok(Object::Native(native))
                if native.target() == CallableTarget::Intrinsic(*THEN_ID)))
    }

    pub(crate) fn call_promise_static(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match name {
            "resolve" => {
                if !matches!(receiver.view(), ValueRef::Object(_)) {
                    return self.throw_type_error(roots, "incompatible Promise receiver");
                }
                let value = args.into_iter().next().unwrap_or(Value::UNDEFINED);
                self.promise_resolve_with(host, roots, receiver, value)
            }
            "reject" => {
                let reason = args.into_iter().next().unwrap_or(Value::UNDEFINED);
                let roots = &roots.with(&receiver);
                let (result, _, reject) =
                    match self.new_promise_capability(host, &roots.with(&reason), receiver)? {
                        Ok(capability) => capability,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                self.call_promise_capability(host, roots, result, &reject, reason)
            }
            "all" => self.combine(host, roots, receiver, CombineKind::All, args),
            "allSettled" => self.combine(host, roots, receiver, CombineKind::AllSettled, args),
            "race" => self.combine(host, roots, receiver, CombineKind::Race, args),
            "any" => self.combine(host, roots, receiver, CombineKind::Any, args),
            _ => self.throw_type_error(roots, &format!("Promise.{name} is not a function")),
        }
    }

    pub(crate) fn call_promise_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        match name {
            "catch" => self.invoke_promise_then(
                host,
                roots,
                receiver,
                vec![
                    Value::UNDEFINED,
                    args.into_iter().next().unwrap_or(Value::UNDEFINED),
                ],
            ),
            "then" | "finally" => {
                let ValueRef::Object(id) = receiver.view() else {
                    return self.throw_type_error(roots, "incompatible Promise receiver");
                };
                if name == "then" && self.heap.get(id)?.as_promise().is_none() {
                    return self.throw_type_error(roots, "incompatible Promise receiver");
                }
                let species = match self.promise_species(host, &roots.with(&args), receiver)? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if name == "finally" {
                    let handler = args.into_iter().next().unwrap_or(Value::UNDEFINED);
                    let handlers = if self.is_callable(&handler) {
                        [false, true]
                            .into_iter()
                            .map(|rejected| {
                                self.alloc_promise_callback(PromiseCallback::Finally {
                                    handler: handler.clone(),
                                    species: species.clone(),
                                    rejected,
                                })
                            })
                            .collect()
                    } else {
                        vec![handler.clone(), handler]
                    };
                    return self.invoke_promise_then(host, roots, receiver, handlers);
                }
                let (result, resolve, reject) =
                    match self.new_promise_capability(host, &roots.with(&args), &species)? {
                        Ok(capability) => capability,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                self.react(
                    id,
                    ReactionJob::Script {
                        resolve,
                        reject,
                        on_fulfilled: self.handler_arg(args.first()),
                        on_rejected: self.handler_arg(args.get(1)),
                    },
                )?;
                Ok(Invocation::Value(result))
            }
            _ => self.throw_type_error(roots, &format!("Promise.{name} is not a function")),
        }
    }

    pub(crate) fn invoke_promise_then(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let result = self.read_property(
            host,
            &roots.with(&(&args, receiver)),
            receiver,
            PropertyKeyRef::String("then"),
        )?;
        match result {
            Invocation::Value(then) => {
                self.invoke(host, roots, &then, Arguments::Values(&args), receiver)
            }
            thrown => Ok(thrown),
        }
    }

    pub(crate) fn promise_species(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let intrinsic = Value::object(self.builtins.get("promise"));
        let constructor = match self.read_property(
            host,
            roots,
            receiver,
            PropertyKeyRef::String("constructor"),
        )? {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Undefined) => {
                return Ok(Invocation::Value(intrinsic));
            }
            Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => value,
            Invocation::Value(_) => {
                return self.throw_type_error(roots, "Promise constructor is not an object");
            }
            thrown => return Ok(thrown),
        };
        match self.read_property(
            host,
            roots,
            &constructor,
            PropertyKeyRef::Symbol(SPECIES_SYMBOL),
        )? {
            Invocation::Value(value)
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
            {
                Ok(Invocation::Value(intrinsic))
            }
            Invocation::Value(value) if !self.is_constructor(&value)? => {
                self.throw_type_error(roots, "Promise species is not a constructor")
            }
            other => Ok(other),
        }
    }

    pub(crate) fn new_promise_capability(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        species: &Value,
    ) -> Result<Result<(Value, Value, Value), Value>, VmError> {
        if species.view() == ValueRef::Object(self.builtins.get("promise")) {
            let result = self.heap.alloc_promise();
            let (resolve, reject) = self.alloc_resolution_pair(result, false)?;
            return Ok(Ok((
                Value::object(result),
                Value::object(resolve),
                Value::object(reject),
            )));
        }
        let executor = self.alloc_promise_callback(PromiseCallback::Executor(None));
        let ValueRef::Object(id) = executor.view() else {
            unreachable!()
        };
        let roots = &roots.with(&id);
        let result = match self.construct_with(
            host,
            roots,
            species,
            Arguments::Values(&[executor]),
            species,
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if let Object::PromiseCallback {
            callback: PromiseCallback::Executor(Some((resolve, reject))),
            ..
        } = self.heap.get(id)?
            && matches!(result.view(), ValueRef::Object(_))
            && self.is_callable(resolve)
            && self.is_callable(reject)
        {
            return Ok(Ok((result, resolve.clone(), reject.clone())));
        }
        Ok(Err(
            self.type_error(roots, "invalid Promise species capability")?
        ))
    }

    pub(crate) fn alloc_promise_callback(&mut self, callback: PromiseCallback) -> Value {
        let length = match callback {
            PromiseCallback::Executor(_) => 2.0,
            PromiseCallback::Finally { .. } | PromiseCallback::CombineElement { .. } => 1.0,
            PromiseCallback::Restore { .. } => 0.0,
        };
        Value::object(self.heap.alloc(Object::PromiseCallback {
            object: PlainObject::callable(self.heap.function_prototype, length, "".into()),
            callback,
        }))
    }

    pub(crate) fn call_promise_callback(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: Arguments<'_>,
    ) -> Result<Invocation, VmError> {
        let argument = args.get(0)?.clone();
        let second = args.get(1)?.clone();
        let Object::PromiseCallback { callback, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        match callback {
            PromiseCallback::Executor(values) => {
                if values.as_ref().is_some_and(|(resolve, reject)| {
                    *resolve != Value::UNDEFINED || *reject != Value::UNDEFINED
                }) {
                    return self.throw_type_error(
                        roots,
                        "Promise capability executor already initialized",
                    );
                }
                *values = Some((argument, second));
                Ok(Invocation::Value(Value::UNDEFINED))
            }
            PromiseCallback::Restore { value, rejected } => Ok(if *rejected {
                Invocation::Thrown(value.clone())
            } else {
                Invocation::Value(value.clone())
            }),
            PromiseCallback::CombineElement {
                state,
                index,
                rejected,
            } => {
                let (state, index, rejected) = (*state, *index, *rejected);
                self.call_combine_element(host, roots, state, index, rejected, argument)
            }
            PromiseCallback::Finally {
                handler,
                species,
                rejected,
            } => {
                let (handler, species, rejected) = (handler.clone(), species.clone(), *rejected);
                let parent_roots = roots;
                let root_view = (&species, &argument);
                let roots = &roots.with(&root_view);
                let value = match self.invoke(
                    host,
                    roots,
                    &handler,
                    Arguments::EMPTY,
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                let cleanup = match self.promise_resolve_with(host, roots, &species, value)? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                let restore = self.alloc_promise_callback(PromiseCallback::Restore {
                    value: argument,
                    rejected,
                });
                self.invoke_promise_then(host, parent_roots, &cleanup, vec![restore])
            }
        }
    }

    pub(crate) fn promise_resolve_with(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        species: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&species);
        if matches!(value.view(), ValueRef::Object(id) if self.heap.promise(id).is_ok()) {
            match self.read_property(host, roots, &value, PropertyKeyRef::String("constructor"))? {
                Invocation::Value(constructor) if constructor == *species => {
                    return Ok(Invocation::Value(value));
                }
                Invocation::Value(_) => {}
                thrown => return Ok(thrown),
            }
        }
        let (result, resolve, _) =
            match self.new_promise_capability(host, &roots.with(&value), species)? {
                Ok(capability) => capability,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        self.call_promise_capability(host, roots, result, &resolve, value)
    }

    pub(super) fn call_promise_capability(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        result: Value,
        callback: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        match self.invoke(
            host,
            &roots.with(&result),
            callback,
            Arguments::Values(&[value]),
            &Value::UNDEFINED,
        )? {
            Invocation::Value(_) => Ok(Invocation::Value(result)),
            thrown => Ok(thrown),
        }
    }

    pub(crate) fn handler_arg(&self, value: Option<&Value>) -> Option<Value> {
        value.filter(|value| self.is_callable(value)).cloned()
    }
}
