//! Resumable Array.fromAsync, including the implicit async-from-sync adapter.
//! Each suspension is a Promise reaction; no native stack survives an await.

use super::*;
use crate::gc::{TraceRoots, Tracer};
use crate::heap::{ASYNC_ITERATOR_SYMBOL, Heap, OwnedEdge, PromiseState, ReactionJob, push_value};
use crate::resolve::AwaitPrepared;
use crate::runtime::Settlement;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayFromAsyncJob(Job);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Job {
    Resume {
        state: State,
        stage: Stage,
    },
    Close {
        result: ObjectId,
        reason: Value,
    },
    Unwrap {
        wrapper: ObjectId,
        done: bool,
        close_on_reject: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    result: ObjectId,
    output: ObjectId,
    source: Source,
    index: u64,
    mapping: Option<(Value, Value)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Iterator {
        iterator: Value,
        next: Value,
        sync: bool,
    },
    ArrayLike {
        object: ObjectId,
        length: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Next,
    Input,
    Mapped,
}

impl TraceRoots for State {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        (self.result, self.output, &self.mapping).trace(tracer)?;
        self.source.trace(tracer)
    }
}

impl TraceRoots for ArrayFromAsyncJob {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match &self.0 {
            Job::Resume { state, .. } => state.trace(tracer),
            Job::Close { result, reason } => (result, reason).trace(tracer),
            Job::Unwrap {
                wrapper,
                close_on_reject,
                ..
            } => (wrapper, close_on_reject).trace(tracer),
        }
    }
}

impl ArrayFromAsyncJob {
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        match &self.0 {
            Job::Resume { state, .. } => {
                // Both kinds of edge share one output buffer.
                out.push(OwnedEdge::Object(state.result));
                out.push(OwnedEdge::Object(state.output));
                match &state.source {
                    Source::ArrayLike { object, .. } => out.push(OwnedEdge::Object(*object)),
                    Source::Iterator { iterator, next, .. } => {
                        push_value(out, iterator);
                        push_value(out, next);
                    }
                }
                if let Some((mapper, receiver)) = &state.mapping {
                    push_value(out, mapper);
                    push_value(out, receiver);
                }
            }
            Job::Close { result, reason } => {
                out.push(OwnedEdge::Object(*result));
                push_value(out, reason);
            }
            Job::Unwrap {
                wrapper,
                close_on_reject,
                ..
            } => {
                out.push(OwnedEdge::Object(*wrapper));
                if let Some(iterator) = close_on_reject {
                    push_value(out, iterator);
                }
            }
        }
    }

    pub(crate) fn validate(&self, heap: &Heap) -> Option<ObjectId> {
        let is_object = |id| {
            heap.get(id).is_ok_and(|o| {
                !matches!(
                    o,
                    Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_)
                )
            })
        };
        let is_iterator = |v: &Value| matches!(v.view(), ValueRef::Object(id) if is_object(id));
        let target = match &self.0 {
            Job::Close { result, .. } => *result,
            Job::Unwrap {
                wrapper,
                done,
                close_on_reject,
            } => {
                if close_on_reject
                    .as_ref()
                    .is_some_and(|v| *done || !is_iterator(v))
                {
                    return None;
                }
                *wrapper
            }
            Job::Resume { state, stage } => {
                if !is_object(state.output)
                    || state.mapping.as_ref().is_some_and(|(v, _)| {
                        !matches!(v.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(crate::builtin::is_callable_object))
                    })
                    || matches!(stage, Stage::Mapped) && state.mapping.is_none()
                {
                    return None;
                }
                match &state.source {
                    Source::Iterator { iterator, .. }
                        if is_iterator(iterator)
                            && state.index < MAX_SAFE_LENGTH
                            && !matches!(stage, Stage::Input) => {}
                    Source::ArrayLike { object, length }
                        if is_object(*object)
                            && state.index < *length
                            && *length <= MAX_SAFE_LENGTH
                            && !matches!(stage, Stage::Next) => {}
                    _ => return None,
                }
                state.result
            }
        };
        let promise = heap.promise(target).ok()?;
        (matches!(promise.state, PromiseState::Pending)
            && !promise.locked
            && promise.adopting.is_none())
        .then_some(target)
    }
}

struct ArrayAsync<'a> {
    vm: &'a mut Vm,
    host: &'a mut dyn Host,
    roots: &'a ActiveRoots<'a>,
}

impl Vm {
    pub(super) fn array_from_async(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        ctor: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_promise();
        let inputs = (ctor, args, result);
        let roots = &roots.with(&inputs);
        let mut cx = ArrayAsync {
            vm: self,
            host,
            roots,
        };
        match cx.start(result, ctor, args)? {
            Ok(state) => cx.step(state)?,
            Err(reason) => cx.reject(result, reason)?,
        }
        Ok(Invocation::Value(Value::object(result)))
    }

    pub(crate) fn resume_array_from_async(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        job: &ArrayFromAsyncJob,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let mut cx = ArrayAsync {
            vm: self,
            host,
            roots,
        };
        match &job.0 {
            Job::Close { result, reason } => cx.reject(*result, reason.clone()),
            Job::Unwrap {
                wrapper,
                done,
                close_on_reject,
            } => match settlement {
                Settlement::Fulfilled(value) => {
                    let result = cx.vm.iterator_result_value(value.clone(), *done)?;
                    cx.vm.resolve_promise(cx.host, cx.roots, *wrapper, result)
                }
                Settlement::Rejected(reason) => {
                    if let Some(iterator) = close_on_reject {
                        cx.vm.close_iterator(cx.host, cx.roots, iterator)?;
                    }
                    cx.reject(*wrapper, reason.clone())
                }
            },
            Job::Resume { state, stage } => match (stage, settlement) {
                (Stage::Mapped, Settlement::Rejected(reason)) => cx.close(state, reason.clone()),
                (_, Settlement::Rejected(reason)) => cx.reject(state.result, reason.clone()),
                (Stage::Next, Settlement::Fulfilled(result)) => cx.next_ready(state, result),
                (Stage::Input, Settlement::Fulfilled(value)) => {
                    cx.value_ready(state, value.clone())
                }
                (Stage::Mapped, Settlement::Fulfilled(value)) => cx.define(state, value.clone()),
            },
        }
    }
}

impl ArrayAsync<'_> {
    fn reject(&mut self, result: ObjectId, reason: Value) -> Result<(), VmError> {
        self.vm.settle(result, Settlement::Rejected(reason))
    }

    fn method(&mut self, object: &Value, key: PropertyKeyRef<'_>) -> Completion<Option<Value>> {
        let method = script!(completion(
            self.vm.read_property(self.host, self.roots, object, key)
        ));
        if matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
            Ok(Ok(None))
        } else if self.vm.is_callable(&method) {
            Ok(Ok(Some(method)))
        } else {
            Ok(Err(self.vm.type_error(
                self.roots,
                "Array.fromAsync iterator method is not callable",
            )?))
        }
    }

    fn start(&mut self, result: ObjectId, ctor: &Value, args: &[Value]) -> Completion<State> {
        let mapper = arg(args, 1);
        let mapping = if matches!(mapper.view(), ValueRef::Undefined) {
            None
        } else if self.vm.is_callable(mapper) {
            Some((mapper.clone(), arg(args, 2).clone()))
        } else {
            return Ok(Err(self.vm.type_error(
                self.roots,
                "Array.fromAsync mapper is not callable",
            )?));
        };
        let items = arg(args, 0);
        if matches!(items.view(), ValueRef::Null | ValueRef::Undefined) {
            return Ok(Err(self
                .vm
                .type_error(self.roots, "Array.fromAsync requires items")?));
        }
        let (method, sync) =
            match script!(self.method(items, PropertyKeyRef::Symbol(ASYNC_ITERATOR_SYMBOL))) {
                Some(method) => (Some(method), false),
                None => (
                    script!(self.method(items, PropertyKeyRef::Symbol(ITERATOR_SYMBOL))),
                    true,
                ),
            };
        let source = if let Some(method) = method {
            let (iterator, next) = script!(self.vm.get_iterator_from_method(
                self.host,
                &self.roots.with(&method),
                items,
                &method,
            ));
            Source::Iterator {
                iterator,
                next,
                sync,
            }
        } else {
            let object = self.vm.coercible_object(items);
            let length = script!(self.vm.length_of_array_like(
                self.host,
                &self.roots.with(&object),
                object
            ));
            Source::ArrayLike { object, length }
        };
        let (length, ctor_args) = match source {
            Source::Iterator { .. } => (0, vec![]),
            Source::ArrayLike { length, .. } => (length, vec![number(length)]),
        };
        let roots = &self.roots.with(&source);
        let output = if self.vm.is_constructor(ctor)? {
            script!(
                self.vm
                    .construct_array_result(self.host, roots, ctor, ctor_args)
            )
            .id
        } else {
            script!(self.vm.array_create(roots, length))
        };
        Ok(Ok(State {
            result,
            output,
            source,
            index: 0,
            mapping,
        }))
    }

    fn wait(&mut self, job: Job, value: Value) -> Completion<()> {
        let job = ArrayFromAsyncJob(job);
        let awaited = (&job, &value);
        let roots = &self.roots.with(&awaited);
        let input = script!(self.vm.prepare_await(self.host, roots, value.clone()));
        let job = ReactionJob::ArrayFromAsync(job);
        match input {
            AwaitPrepared::Plain(value) => {
                self.vm.enqueue_job(job, Settlement::Fulfilled(value))?
            }
            AwaitPrepared::Promise(promise) => self.vm.react(promise, job)?,
        }
        Ok(Ok(()))
    }

    fn step(&mut self, state: State) -> Result<(), VmError> {
        let roots = &self.roots.with(&state);
        let mut cx = ArrayAsync {
            vm: self.vm,
            host: self.host,
            roots,
        };
        let result = state.result;
        let step = match &state.source {
            Source::Iterator {
                iterator,
                next,
                sync,
            } => {
                if state.index == MAX_SAFE_LENGTH {
                    let reason = cx
                        .vm
                        .type_error(roots, "Array.fromAsync exceeds the safe integer limit")?;
                    return cx.close(&state, reason);
                }
                let invocation = cx
                    .vm
                    .invoke(cx.host, roots, next, Arguments::EMPTY, iterator)?;
                let next = if *sync {
                    let wrapper = cx.vm.heap.alloc_promise();
                    cx.sync_wrapper(wrapper, invocation, Some(iterator))?;
                    Ok(Value::object(wrapper))
                } else {
                    completion(Ok(invocation))?
                };
                next.map(|value| (Stage::Next, value))
            }
            Source::ArrayLike { object, length } => {
                if state.index == *length {
                    return cx.finish(&state);
                }
                completion(
                    cx.vm
                        .read_array_like_index(cx.host, roots, *object, state.index),
                )?
                .map(|value| (Stage::Input, value))
            }
        };
        // The new job roots the state during await preparation.
        let step = match step {
            Ok((stage, value)) => self.wait(Job::Resume { state, stage }, value)?,
            Err(reason) => Err(reason),
        };
        if let Err(reason) = step {
            self.reject(result, reason)?;
        }
        Ok(())
    }

    fn read_result(&mut self, result: &Value, read_done_value: bool) -> Completion<(bool, Value)> {
        if !matches!(result.view(), ValueRef::Object(_)) {
            return Ok(Err(self
                .vm
                .type_error(self.roots, "iterator result is not an object")?));
        }
        let roots = &self.roots.with(result);
        let done = script!(completion(self.vm.read_property(
            self.host,
            roots,
            result,
            PropertyKeyRef::String("done")
        )))
        .truthy();
        let value = if !done || read_done_value {
            script!(completion(self.vm.read_property(
                self.host,
                roots,
                result,
                PropertyKeyRef::String("value")
            )))
        } else {
            Value::UNDEFINED
        };
        Ok(Ok((done, value)))
    }

    fn next_ready(&mut self, state: &State, next: &Value) -> Result<(), VmError> {
        match self.read_result(next, false)? {
            Ok((true, _)) => self.finish(state),
            Ok((false, value)) => self.value_ready(state, value),
            Err(reason) => self.reject(state.result, reason),
        }
    }

    fn value_ready(&mut self, state: &State, value: Value) -> Result<(), VmError> {
        let Some((mapper, receiver)) = &state.mapping else {
            return self.define(state, value);
        };
        let args = [value, number(state.index)];
        let mapped = self.vm.invoke(
            self.host,
            &self.roots.with(&args),
            mapper,
            Arguments::Values(&args),
            receiver,
        )?;
        let outcome = match mapped {
            Invocation::Thrown(reason) => Err(reason),
            Invocation::Value(value) => self.wait(
                Job::Resume {
                    state: state.clone(),
                    stage: Stage::Mapped,
                },
                value,
            )?,
        };
        match outcome {
            Ok(()) => Ok(()),
            Err(reason) => self.close(state, reason),
        }
    }

    fn define(&mut self, state: &State, value: Value) -> Result<(), VmError> {
        let mut output = ArrayAccess {
            vm: self.vm,
            host: self.host,
            roots: self.roots,
            id: state.output,
            len: state.index,
        };
        match output.define(
            ArrayResult {
                id: state.output,
                fresh: false,
            },
            state.index,
            value,
        )? {
            Ok(()) => self.step(State {
                index: state.index + 1,
                ..state.clone()
            }),
            Err(reason) => self.close(state, reason),
        }
    }

    fn finish(&mut self, state: &State) -> Result<(), VmError> {
        let mut output = ArrayAccess {
            vm: self.vm,
            host: self.host,
            roots: self.roots,
            id: state.output,
            len: state.index,
        };
        match output.set("length".into(), number(state.index))? {
            Ok(_) => self.vm.resolve_promise(
                self.host,
                self.roots,
                state.result,
                Value::object(state.output),
            ),
            Err(reason) => self.reject(state.result, reason),
        }
    }

    fn close(&mut self, state: &State, reason: Value) -> Result<(), VmError> {
        let Source::Iterator { iterator, sync, .. } = &state.source else {
            return self.reject(state.result, reason);
        };
        let roots = &self.roots.with(&reason);
        let mut cx = ArrayAsync {
            vm: self.vm,
            host: self.host,
            roots,
        };
        let method = cx.method(iterator, PropertyKeyRef::String("return"))?;
        if !sync && !matches!(method, Ok(Some(_))) {
            return self.reject(state.result, reason);
        }
        let invocation = match method {
            Ok(None) => None,
            Ok(Some(method)) => Some(cx.vm.invoke(
                cx.host,
                &roots.with(&method),
                &method,
                Arguments::EMPTY,
                iterator,
            )?),
            Err(error) => Some(Invocation::Thrown(error)),
        };
        let awaited = if *sync {
            let wrapper = cx.vm.heap.alloc_promise();
            if let Some(invocation) = invocation {
                cx.sync_wrapper(wrapper, invocation, None)?;
            } else {
                let result = cx.vm.iterator_result_value(Value::UNDEFINED, true)?;
                cx.vm.resolve_promise(cx.host, roots, wrapper, result)?;
            }
            Value::object(wrapper)
        } else if let Some(Invocation::Value(value)) = invocation {
            value
        } else {
            return self.reject(state.result, reason);
        };
        if self
            .wait(
                Job::Close {
                    result: state.result,
                    reason: reason.clone(),
                },
                awaited,
            )?
            .is_err()
        {
            self.reject(state.result, reason)?;
        }
        Ok(())
    }

    // The sync adapter adds a separate reaction turn, even for plain values and
    // done:true. Its return operation does not close again on rejection.
    fn sync_wrapper(
        &mut self,
        wrapper: ObjectId,
        invocation: Invocation,
        iterator: Option<&Value>,
    ) -> Result<(), VmError> {
        let result = match invocation {
            Invocation::Value(result) => result,
            Invocation::Thrown(reason) => return self.reject(wrapper, reason),
        };
        let inputs = (wrapper, &result);
        let roots = &self.roots.with(&inputs);
        let mut cx = ArrayAsync {
            vm: self.vm,
            host: self.host,
            roots,
        };
        let (done, value) = match cx.read_result(&result, true)? {
            Ok(result) => result,
            Err(reason) => return cx.reject(wrapper, reason),
        };
        let close_on_reject = if done { None } else { iterator.cloned() };
        let job = Job::Unwrap {
            wrapper,
            done,
            close_on_reject,
        };
        if let Err(reason) = cx.wait(job, value)? {
            if !done && let Some(iterator) = iterator {
                cx.vm
                    .close_iterator(cx.host, &roots.with(&reason), iterator)?;
            }
            cx.reject(wrapper, reason)?;
        }
        Ok(())
    }
}

impl TraceRoots for Source {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Self::Iterator { iterator, next, .. } => (iterator, next).trace(tracer),
            Self::ArrayLike { object, .. } => object.trace(tracer),
        }
    }
}
