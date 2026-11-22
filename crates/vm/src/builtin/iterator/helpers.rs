//! Iterator helper state and protocol operations.

use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::error::VmError;
use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{Heap, ITERATOR_SYMBOL, Object, PlainObject};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsBigInt, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub(super) iterator: Value,
    pub(super) next: Value,
}

impl Record {
    pub(crate) fn values(&self) -> [&Value; 2] {
        [&self.iterator, &self.next]
    }

    pub(crate) fn valid(&self, heap: &Heap) -> bool {
        matches!(self.iterator.view(), ValueRef::Object(id) if script_object(heap, id))
            && !matches!(self.next.view(), ValueRef::Object(id) if !script_object(heap, id))
    }
}

fn script_object(heap: &Heap, id: crate::value::ObjectId) -> bool {
    matches!(heap.get(id), Ok(object) if !matches!(object,
        Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_)))
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Method {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
    Reduce,
    ToArray,
    ForEach,
    Some,
    Every,
    Find,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Limit {
    Small(u64),
    Large(JsBigInt),
    Infinite,
}

impl Limit {
    fn new(number: f64) -> Self {
        if number.is_infinite() {
            Self::Infinite
        } else if let Some(n) = number.to_u64() {
            Self::Small(n)
        } else {
            Self::Large(JsBigInt::from_f64(number).expect("finite integral limit"))
        }
    }

    /// Consumes one unit of the budget, returning false if it is exhausted.
    fn tick(&mut self) -> bool {
        match self {
            Self::Small(0) => return false,
            Self::Small(n) => *n -= 1,
            Self::Large(n) => {
                let next = n.as_bigint() - 1u8;
                *self = next
                    .to_u64()
                    .map_or_else(|| Self::Large(JsBigInt::from_bigint(next)), Self::Small);
            }
            Self::Infinite => {}
        }
        true
    }

    fn valid(&self) -> bool {
        !matches!(self, Self::Large(n) if n.as_bigint().sign() != num_bigint::Sign::Plus || n.as_bigint().to_u64().is_some())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Operation {
    Map(Value),
    Filter(Value),
    Take(Limit),
    Drop(Limit),
    FlatMap(Value),
}

impl Operation {
    fn callback(&self) -> Option<&Value> {
        match self {
            Self::Map(callback) | Self::Filter(callback) | Self::FlatMap(callback) => {
                Some(callback)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    outer: Record,
    operation: Operation,
    inner: Option<Record>,
    index: u64,
    started: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConcatInput {
    iterable: Value,
    open: Value,
}

impl TraceRoots for ConcatInput {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        [&self.iterable, &self.open].trace(tracer)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcatCursor {
    // Stored in reverse order so completing each open can release that entry.
    pending: Vec<ConcatInput>,
    current: Option<Record>,
}

impl ConcatCursor {
    fn values(&self) -> impl Iterator<Item = &Value> {
        self.pending
            .iter()
            .flat_map(|input| [&input.iterable, &input.open])
            .chain(self.current.iter().flat_map(Record::values))
    }

    fn valid(&self, heap: &Heap) -> bool {
        self.current.as_ref().is_none_or(|record| record.valid(heap))
            && self.pending.iter().all(|input| {
                matches!(input.iterable.view(), ValueRef::Object(id) if script_object(heap, id))
                    && matches!(input.open.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(crate::builtin::is_callable_object))
            })
    }
}

impl TraceRoots for ConcatCursor {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        for value in self.values() {
            value.trace(tracer)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HelperState {
    Suspended(Cursor),
    Concat(ConcatCursor),
    Running,
    Done,
}

impl HelperState {
    pub(crate) fn values(&self) -> impl Iterator<Item = &Value> {
        let cursor = match self {
            Self::Suspended(cursor) => Some(cursor),
            _ => None,
        };
        let concat = match self {
            Self::Concat(cursor) => Some(cursor),
            _ => None,
        };
        cursor
            .into_iter()
            .flat_map(|cursor| {
                cursor
                    .outer
                    .values()
                    .into_iter()
                    .chain(cursor.operation.callback())
                    .chain(cursor.inner.iter().flat_map(Record::values))
            })
            .chain(concat.into_iter().flat_map(ConcatCursor::values))
    }

    pub(crate) fn valid(&self, heap: &Heap) -> bool {
        match self {
            Self::Running => false,
            Self::Done => true,
            Self::Concat(cursor) => cursor.valid(heap),
            Self::Suspended(cursor) => {
                cursor.outer.valid(heap)
                    && (cursor.started || cursor.index == 0)
                    && cursor.operation.callback().is_none_or(|callback| {
                        matches!(callback.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(crate::builtin::is_callable_object))
                    })
                    && cursor.inner.as_ref().is_none_or(|inner| {
                        cursor.started && matches!(cursor.operation, Operation::FlatMap(_))
                            && inner.valid(heap)
                    })
                    && match &cursor.operation {
                        Operation::Take(limit) | Operation::Drop(limit) => limit.valid(),
                        _ => true,
                    }
            }
        }
    }
}

/// Only the outer VM error bypasses JavaScript abrupt-completion handling.
macro_rules! script {
    ($expr:expr) => {
        match $expr? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
    };
}

pub(super) struct Protocol<'a, 'r> {
    pub(super) vm: &'a mut Vm,
    pub(super) host: &'a mut dyn Host,
    pub(super) roots: &'a ActiveRoots<'r>,
}

impl Protocol<'_, '_> {
    fn direct(&mut self, iterator: &Value) -> Result<Result<Record, Value>, VmError> {
        match self.vm.read_property(
            self.host,
            self.roots,
            iterator,
            PropertyKeyRef::String("next"),
        )? {
            Invocation::Value(next) => Ok(Ok(Record {
                iterator: iterator.clone(),
                next,
            })),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    pub(super) fn flatten(
        &mut self,
        mapped: &Value,
        allow_strings: bool,
    ) -> Result<Result<Record, Value>, VmError> {
        let is_object = matches!(mapped.view(), ValueRef::Object(_));
        if !(is_object || allow_strings && matches!(mapped.view(), ValueRef::String(_))) {
            let message = if allow_strings {
                "Iterator.from input must be an object or a string"
            } else {
                "flatMap mapper must return an object"
            };
            return Ok(Err(self.vm.type_error(self.roots, message)?));
        }
        let roots = &self.roots.with(mapped);
        let method = match self.vm.read_property(
            self.host,
            roots,
            mapped,
            PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
        )? {
            Invocation::Thrown(error) => return Ok(Err(error)),
            Invocation::Value(method) => method,
        };
        if matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
            if !is_object {
                return Ok(Err(self
                    .vm
                    .type_error(roots, "iterator must be an object")?));
            }
            self.direct(mapped)
        } else {
            self.vm
                .get_iterator_from_method(self.host, roots, mapped, &method)
                .map(|result| result.map(|(iterator, next)| Record { iterator, next }))
        }
    }

    fn close(&mut self, iterator: &Value, completion: Invocation) -> Result<Invocation, VmError> {
        let value = match &completion {
            Invocation::Value(value) | Invocation::Thrown(value) => value,
        };
        let closed = self
            .vm
            .close_iterator(self.host, &self.roots.with(value), iterator)?;
        Ok(match (&completion, closed) {
            (Invocation::Value(_), thrown @ Invocation::Thrown(_)) => thrown,
            _ => completion,
        })
    }

    fn step(&mut self, record: &Record) -> Result<Result<Option<Value>, Value>, VmError> {
        self.vm
            .iterator_step(self.host, self.roots, &record.iterator, &record.next)
    }

    fn call(&mut self, callback: &Value, args: &[Value]) -> Result<Invocation, VmError> {
        self.vm.invoke(
            self.host,
            self.roots,
            callback,
            Arguments::Values(args),
            &Value::UNDEFINED,
        )
    }
}

pub(super) fn concat(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
) -> Result<Invocation, VmError> {
    let mut pending = Vec::with_capacity(args.len());
    let mut args = args.into_iter();
    while let Some(iterable) = args.next() {
        let edges = (&iterable, args.as_slice(), &pending);
        let roots = &roots.with(&edges);
        if !matches!(iterable.view(), ValueRef::Object(_)) {
            return vm.throw_type_error(roots, "Iterator.concat inputs must be objects");
        }
        let open = match vm.read_property(
            host,
            roots,
            &iterable,
            PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
        )? {
            Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
            Invocation::Value(open) => open,
        };
        if !vm.is_callable(&open) {
            return vm.throw_type_error(
                roots,
                "Iterator.concat input must have a callable Symbol.iterator",
            );
        }
        pending.push(ConcatInput { iterable, open });
    }
    pending.reverse();
    let id = vm.heap.alloc(Object::IteratorHelper {
        object: PlainObject::new(Some(vm.builtins.get(super::HELPER_ROOT))),
        state: HelperState::Concat(ConcatCursor {
            pending,
            current: None,
        }),
    });
    Ok(Invocation::Value(Value::object(id)))
}

pub(super) fn invoke(
    method: Method,
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Arguments<'_>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if !matches!(receiver.view(), ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "iterator receiver must be an object");
    }
    let invocation = (receiver, args);
    let roots = &roots.with(&invocation);
    let mut protocol = Protocol { vm, host, roots };
    let callback = args.get(0)?;
    let operation = match method {
        Method::Take | Method::Drop => {
            let number = match protocol.vm.coerce_number(protocol.host, roots, callback)? {
                Ok(number) => number,
                Err(error) => return protocol.close(receiver, Invocation::Thrown(error)),
            };
            let integer = number.trunc();
            if integer.is_nan() || integer < 0.0 {
                let error = protocol
                    .vm
                    .make_range_error(roots, "invalid iterator limit")?;
                return protocol.close(receiver, Invocation::Thrown(error));
            }
            let limit = Limit::new(integer);
            Some(if method == Method::Take {
                Operation::Take(limit)
            } else {
                Operation::Drop(limit)
            })
        }
        _ => {
            if method != Method::ToArray && !protocol.vm.is_callable(callback) {
                let error = protocol
                    .vm
                    .type_error(roots, "iterator callback is not callable")?;
                return protocol.close(receiver, Invocation::Thrown(error));
            }
            match method {
                Method::Map => Some(Operation::Map(callback.clone())),
                Method::Filter => Some(Operation::Filter(callback.clone())),
                Method::FlatMap => Some(Operation::FlatMap(callback.clone())),
                _ => None,
            }
        }
    };
    let outer = script!(protocol.direct(receiver));
    if let Some(operation) = operation {
        let id = protocol.vm.heap.alloc(Object::IteratorHelper {
            object: PlainObject::new(Some(protocol.vm.builtins.get(super::HELPER_ROOT))),
            state: HelperState::Suspended(Cursor {
                outer,
                operation,
                inner: None,
                index: 0,
                started: false,
            }),
        });
        return Ok(Invocation::Value(Value::object(id)));
    }
    consume(protocol, method, &outer, callback, args)
}

fn consume(
    protocol: Protocol<'_, '_>,
    method: Method,
    outer: &Record,
    callback: &Value,
    args: Arguments<'_>,
) -> Result<Invocation, VmError> {
    let edges = outer.values();
    let roots = &protocol.roots.with(&edges);
    let mut protocol = Protocol {
        vm: protocol.vm,
        host: protocol.host,
        roots,
    };
    let mut index = 0u64;
    let mut accumulator = Value::UNDEFINED;
    let mut items = Vec::new();
    if method == Method::Reduce {
        accumulator = if args.len() > 1 {
            args.get(1)?.clone()
        } else {
            index = 1;
            let Some(value) = script!(protocol.step(outer)) else {
                return protocol.vm.throw_type_error(
                    roots,
                    "cannot reduce an empty iterator without an initial value",
                );
            };
            value
        };
    }
    loop {
        let partial = (&accumulator, &items);
        let loop_roots = &roots.with(&partial);
        let mut protocol = Protocol {
            vm: protocol.vm,
            host: protocol.host,
            roots: loop_roots,
        };
        let Some(value) = script!(protocol.step(outer)) else {
            break;
        };
        if method == Method::ToArray {
            items.push(value);
            continue;
        }
        let key = Value::number(index as f64);
        let result = if method == Method::Reduce {
            protocol.call(callback, &[accumulator.clone(), value.clone(), key])?
        } else {
            protocol.call(callback, &[value.clone(), key])?
        };
        match result {
            thrown @ Invocation::Thrown(_) => return protocol.close(&outer.iterator, thrown),
            Invocation::Value(result) => match method {
                Method::Reduce => accumulator = result,
                Method::Some if result.truthy() => {
                    return protocol
                        .close(&outer.iterator, Invocation::Value(Value::boolean(true)));
                }
                Method::Every if !result.truthy() => {
                    return protocol
                        .close(&outer.iterator, Invocation::Value(Value::boolean(false)));
                }
                Method::Find if result.truthy() => {
                    return protocol.close(&outer.iterator, Invocation::Value(value));
                }
                _ => {}
            },
        }
        index += 1;
    }
    Ok(Invocation::Value(match method {
        Method::ToArray => Value::object(protocol.vm.heap.alloc_array(items)),
        Method::Reduce => accumulator,
        Method::Some => Value::boolean(false),
        Method::Every => Value::boolean(true),
        _ => Value::UNDEFINED,
    }))
}

pub(super) fn resume(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    returning: bool,
) -> Result<Invocation, VmError> {
    let invalid = "iterator helper method requires an iterator helper";
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, invalid);
    };
    let Object::IteratorHelper { state, .. } = vm.heap.get_mut(id)? else {
        return vm.throw_type_error(roots, invalid);
    };
    let mut suspended = match state {
        HelperState::Running => {
            return vm.throw_type_error(roots, "iterator helper is already running");
        }
        HelperState::Done => return vm.iterator_result(Value::UNDEFINED, true),
        HelperState::Suspended(cursor) if returning && !cursor.started => {
            std::mem::replace(state, HelperState::Done)
        }
        HelperState::Suspended(_) | HelperState::Concat(_) => {
            std::mem::replace(state, HelperState::Running)
        }
    };
    let roots = &roots.with(receiver);
    let mut protocol = Protocol { vm, host, roots };
    let result = match &mut suspended {
        HelperState::Suspended(cursor) if returning => close_cursor(&mut protocol, cursor),
        HelperState::Suspended(cursor) => {
            cursor.started = true;
            advance(&mut protocol, cursor)
        }
        HelperState::Concat(cursor) if returning => close_concat(&mut protocol, cursor),
        HelperState::Concat(cursor) => advance_concat(&mut protocol, cursor),
        _ => unreachable!(),
    };
    let Object::IteratorHelper { state, .. } = protocol.vm.heap.get_mut(id)? else {
        unreachable!()
    };
    *state = if matches!(result, Ok(Outcome::Yield(_))) {
        suspended
    } else {
        HelperState::Done
    };
    match result? {
        Outcome::Yield(value) => protocol.vm.iterator_result(value, false),
        Outcome::Done => protocol.vm.iterator_result(Value::UNDEFINED, true),
        Outcome::Thrown(error) => Ok(Invocation::Thrown(error)),
    }
}

fn advance_concat(
    protocol: &mut Protocol<'_, '_>,
    cursor: &mut ConcatCursor,
) -> Result<Outcome, VmError> {
    loop {
        let roots = &protocol.roots.with(cursor);
        if let Some(record) = &cursor.current {
            let mut step = Protocol {
                vm: protocol.vm,
                host: protocol.host,
                roots,
            };
            match step.step(record)? {
                Ok(Some(value)) => return Ok(Outcome::Yield(value)),
                Ok(None) => cursor.current = None,
                Err(error) => return Ok(Outcome::Thrown(error)),
            }
        } else if let Some(input) = cursor.pending.last() {
            match protocol.vm.get_iterator_from_method(
                protocol.host,
                roots,
                &input.iterable,
                &input.open,
            )? {
                Ok((iterator, next)) => {
                    cursor.pending.pop();
                    cursor.current = Some(Record { iterator, next });
                }
                Err(error) => return Ok(Outcome::Thrown(error)),
            }
        } else {
            return Ok(Outcome::Done);
        }
    }
}

fn close_concat(
    protocol: &mut Protocol<'_, '_>,
    cursor: &ConcatCursor,
) -> Result<Outcome, VmError> {
    let Some(record) = &cursor.current else {
        return Ok(Outcome::Done);
    };
    let roots = &protocol.roots.with(cursor);
    match protocol
        .vm
        .close_iterator(protocol.host, roots, &record.iterator)?
    {
        Invocation::Value(_) => Ok(Outcome::Done),
        Invocation::Thrown(error) => Ok(Outcome::Thrown(error)),
    }
}

fn close_cursor(protocol: &mut Protocol<'_, '_>, cursor: &Cursor) -> Result<Outcome, VmError> {
    // When closing an active flatMap, always close outer after inner, with
    // the first abrupt completion taking precedence.
    let mut completion = Invocation::Value(Value::UNDEFINED);
    if let Some(inner) = cursor.inner.as_ref() {
        let outer = cursor.outer.values();
        let mut inner_protocol = Protocol {
            vm: protocol.vm,
            host: protocol.host,
            roots: &protocol.roots.with(&outer),
        };
        completion = inner_protocol.close(&inner.iterator, completion)?;
    }
    protocol
        .close(&cursor.outer.iterator, completion)
        .map(|completion| match completion {
            Invocation::Value(_) => Outcome::Done,
            Invocation::Thrown(error) => Outcome::Thrown(error),
        })
}

enum Outcome {
    Yield(Value),
    Done,
    Thrown(Value),
}

fn close_abrupt(
    protocol: &mut Protocol<'_, '_>,
    outer: &Record,
    error: Value,
) -> Result<Outcome, VmError> {
    protocol
        .close(&outer.iterator, Invocation::Thrown(error))
        .map(|closed| match closed {
            Invocation::Thrown(error) => Outcome::Thrown(error),
            Invocation::Value(_) => unreachable!(),
        })
}

fn advance(protocol: &mut Protocol<'_, '_>, cursor: &mut Cursor) -> Result<Outcome, VmError> {
    let outer = &cursor.outer;
    let edges = outer.values();
    let roots = &protocol.roots.with(&edges);
    let mut protocol = Protocol {
        vm: protocol.vm,
        host: protocol.host,
        roots,
    };
    match &mut cursor.operation {
        Operation::Drop(limit) => {
            while limit.tick() {
                match protocol.vm.iterator_step_result(
                    protocol.host,
                    roots,
                    &outer.iterator,
                    &outer.next,
                )? {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(Outcome::Done),
                    Err(error) => return Ok(Outcome::Thrown(error)),
                }
            }
        }
        Operation::Take(limit) => {
            if !limit.tick() {
                return protocol
                    .close(&outer.iterator, Invocation::Value(Value::UNDEFINED))
                    .map(|result| match result {
                        Invocation::Value(_) => Outcome::Done,
                        Invocation::Thrown(error) => Outcome::Thrown(error),
                    });
            }
        }
        _ => {}
    }
    let callback = cursor.operation.callback();
    let roots = &roots.with(&callback);
    let mut protocol = Protocol { roots, ..protocol };
    loop {
        if let Some(inner) = cursor.inner.as_ref() {
            let edges = inner.values();
            let mut inner_protocol = Protocol {
                vm: protocol.vm,
                host: protocol.host,
                roots: &roots.with(&edges),
            };
            match inner_protocol.step(inner)? {
                Ok(Some(value)) => return Ok(Outcome::Yield(value)),
                Ok(None) => cursor.inner = None,
                Err(error) => return close_abrupt(&mut protocol, outer, error),
            }
        }
        let value = match protocol.step(outer)? {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(Outcome::Done),
            Err(error) => return Ok(Outcome::Thrown(error)),
        };
        let Some(callback) = callback else {
            return Ok(Outcome::Yield(value));
        };
        let result = protocol.call(
            callback,
            &[value.clone(), Value::number(cursor.index as f64)],
        )?;
        cursor.index += 1;
        let result = match result {
            Invocation::Value(result) => result,
            Invocation::Thrown(error) => return close_abrupt(&mut protocol, outer, error),
        };
        match cursor.operation {
            Operation::Map(_) => return Ok(Outcome::Yield(result)),
            Operation::Filter(_) if result.truthy() => return Ok(Outcome::Yield(value)),
            Operation::Filter(_) => {}
            Operation::FlatMap(_) => match protocol.flatten(&result, false)? {
                Ok(inner) => cursor.inner = Some(inner),
                Err(error) => return close_abrupt(&mut protocol, outer, error),
            },
            _ => unreachable!(),
        }
    }
}
