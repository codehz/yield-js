//! Array builtins use ordinary property operations for observable reads and writes.
//! New results define own data properties, bypassing inherited setters.

use crate::builtin::globals::Owner;
use crate::builtin::{RootKind, RootSpec, define_builtin_data, link_constructor};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ITERATOR_SYMBOL, Object};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops::strict_equal;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

const MAX_LENGTH: usize = u32::MAX as usize;

#[yield_binding_macros::builtin_global(name = "Array", root = "array", kind = "constructor")]
mod members {
    static_calls! {
        isArray | from | of as name => |vm, host, roots, args, receiver| {
            vm.array_operation(host, roots, name, receiver, args)
        };
    }

    prototype_calls! {
        push | pop | shift | unshift | splice | slice | fill | map | filter | reduce | forEach
            | some | every | flat | indexOf | lastIndexOf | includes | concat | find | findIndex
            | flatMap as name => |vm, host, roots, args, receiver| {
                vm.array_operation(host, roots, name, receiver, args)
            };
        join => |vm, host, roots, args, receiver| vm.array_join(host, roots, receiver, &args);
        toString => |vm, host, roots, _, receiver| vm.array_to_string(host, roots, receiver);
    }
}

const ARRAY_CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Array", Slot::Global, "");
const ARRAY_VALUES: IntrinsicKey = IntrinsicKey::new("Array", Slot::Prototype, "values");
const ARRAY_ITERATOR_NEXT: IntrinsicKey =
    IntrinsicKey::new("ArrayIterator", Slot::Prototype, "next");

pub(crate) const OWNER: Owner = Owner::new(
    members::ROOT,
    ARRAY_ROOTS,
    &[ARRAY_INTRINSICS, members::INTRINSICS],
    Some(|vm, identity| {
        vm.install_array(
            vm.builtins.get("array"),
            vm.builtins.get("array.prototype"),
            vm.builtins.get("array.values"),
            vm.builtins.get("arrayIterator.prototype"),
            identity,
        );
    }),
);

pub(crate) const ARRAY_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "array",
        RootKind::Native(ARRAY_CONSTRUCTOR),
        Some("function.prototype"),
    )
    .fixed("array.prototype"),
    RootSpec::new("array.prototype", RootKind::Array, Some("object.prototype")),
    RootSpec::new(
        "array.values",
        RootKind::Native(ARRAY_VALUES),
        Some("function.prototype"),
    ),
    RootSpec::new(
        "arrayIterator.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
];

pub(crate) const ARRAY_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        ARRAY_CONSTRUCTOR,
        |vm, _, _, args, _| vm.construct_array(args),
        |vm, _, _, args, prototype| {
            let result = vm.construct_array(args)?;
            vm.with_result_prototype(result, prototype)
        },
    ),
    IntrinsicDef::call(ARRAY_VALUES, |vm, _, _, _, receiver| {
        vm.array_values(receiver)
    }),
    IntrinsicDef::call(ARRAY_ITERATOR_NEXT, |vm, host, roots, _, receiver| {
        vm.array_iterator_next(host, roots, receiver)
    }),
];

type Completion<T> = Result<Result<T, Value>, VmError>;

// Unpack a property access or script call without converting throws into VM errors.
fn completion(result: Result<Invocation, VmError>) -> Completion<Value> {
    Ok(match result? {
        Invocation::Value(value) => Ok(value),
        Invocation::Thrown(error) => Err(error),
    })
}

macro_rules! script {
    ($expr:expr) => {
        match $expr? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

fn invocation(result: Completion<Value>) -> Result<Invocation, VmError> {
    Ok(match result? {
        Ok(value) => Invocation::Value(value),
        Err(error) => Invocation::Thrown(error),
    })
}

fn number(index: usize) -> Value {
    Value::Number(index as f64)
}

fn arg(args: &[Value], index: usize) -> &Value {
    args.get(index).unwrap_or(&Value::Undefined)
}

fn relative(index: f64, len: usize) -> usize {
    let len = len as f64;
    (if index < 0.0 { len + index } else { index }).clamp(0.0, len) as usize
}

impl Vm {
    pub(crate) fn install_array(
        &mut self,
        constructor: ObjectId,
        prototype: ObjectId,
        values: ObjectId,
        iterator: ObjectId,
        identity: &Value,
    ) {
        link_constructor(self, constructor, prototype);
        self.heap.array_prototype = Some(prototype);
        members::install_members(self, constructor, prototype);
        // `values` is already rooted; both properties must point to that object.
        define_builtin_data(
            self,
            prototype,
            ARRAY_VALUES.member,
            Value::Object(values),
            Attributes::METHOD,
        );
        self.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                Value::Object(values),
                Attributes::METHOD,
            )
            .expect("Array iterator alias");
        let next = self.alloc_intrinsic(ARRAY_ITERATOR_NEXT);
        self.heap
            .define_data(iterator, "next", Value::Object(next), Attributes::METHOD)
            .expect("Array iterator next");
        self.heap
            .define_data(
                iterator,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                identity.clone(),
                Attributes::METHOD,
            )
            .expect("Array iterator identity");
    }

    fn construct_array(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let id = if let [Value::Number(length)] = args.as_slice() {
            if !(0.0..=MAX_LENGTH as f64).contains(length) || length.fract() != 0.0 {
                return self.throw_range_error("invalid array length");
            }
            self.sparse_array(*length as usize)?
        } else {
            self.heap.alloc_array(args)
        };
        Ok(Invocation::Value(Value::Object(id)))
    }

    fn sparse_array(&mut self, length: usize) -> Result<ObjectId, VmError> {
        let id = self.heap.alloc_array(Vec::new());
        let Object::Array(array) = self.heap.get_mut(id)? else {
            unreachable!()
        };
        array.length = length;
        Ok(id)
    }

    fn array_length(&self, value: &Value) -> Result<Option<usize>, VmError> {
        Ok(match value {
            Value::Object(id) => match self.heap.get(*id)? {
                Object::Array(array) => Some(array.length),
                _ => None,
            },
            _ => None,
        })
    }

    fn array_operation(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        name: &str,
        receiver: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match name {
            "of" => {
                return Ok(Invocation::Value(Value::Object(
                    self.heap.alloc_array(args),
                )));
            }
            "isArray" => {
                let is_array = match arg(&args, 0) {
                    Value::Object(id) => matches!(
                        self.heap.get(self.heap.proxy_target(*id)?)?,
                        Object::Array(_)
                    ),
                    _ => false,
                };
                return Ok(Invocation::Value(Value::Bool(is_array)));
            }
            "from" => return invocation(self.array_from(host, roots, &args)),
            _ => {}
        }
        let Some(len) = self.array_length(receiver)? else {
            return self.throw_type_error("Array method requires an array receiver");
        };
        let Value::Object(id) = *receiver else {
            unreachable!()
        };
        let root_view = (receiver, &args);
        let roots = &roots.with(&root_view);
        let mut array = ArrayAccess {
            vm: self,
            host,
            roots,
            id,
            len,
        };
        invocation(match name {
            "push" | "pop" | "shift" | "unshift" | "splice" => array.mutate(name, &args),
            "slice" | "fill" => array.range(name, &args),
            "indexOf" | "lastIndexOf" | "includes" => array.search(name, &args),
            "concat" | "flat" => array.combine(name, &args),
            _ => array.callback(name, &args),
        })
    }

    fn array_from(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Completion<Value> {
        let roots = &roots.with(&args);
        let items = arg(args, 0);
        let mapper = arg(args, 1);
        let mapping = !matches!(mapper, Value::Undefined);
        if mapping && !self.is_callable(mapper) {
            return Ok(Err(self.type_error("Array.from mapper is not callable")?));
        }
        if matches!(items, Value::Null | Value::Undefined) {
            return Ok(Err(self.type_error("Array.from requires items")?));
        }
        let method = match items {
            Value::Object(_) | Value::String(_) => script!(completion(self.read_property(
                host,
                roots,
                items,
                PropertyKeyRef::Symbol(ITERATOR_SYMBOL)
            ))),
            _ => Value::Undefined,
        };
        let mut values = Vec::new();
        if !matches!(method, Value::Null | Value::Undefined) {
            let (iter, next) = script!(self.get_iterator_from_method(host, roots, items, &method));
            let root_view = (&iter, &next);
            let roots = &roots.with(&root_view);
            while let Some(mut value) =
                script!(self.iterator_step(host, &roots.with(&values), &iter, &next))
            {
                let roots = &roots.with(&values);
                if values.len() == MAX_LENGTH {
                    self.close_iterator(host, roots, &iter)?;
                    return Ok(Err(self.make_range_error("invalid array length")?));
                }
                if mapping {
                    value = match self.invoke(
                        host,
                        roots,
                        mapper,
                        vec![value, number(values.len())],
                        arg(args, 2),
                    )? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => {
                            // IteratorClose preserves the original throw.
                            let roots = &roots.with(&error);
                            self.close_iterator(host, roots, &iter)?;
                            return Ok(Err(error));
                        }
                    };
                }
                values.push(value);
            }
        } else if matches!(items, Value::Object(_)) {
            let length = script!(completion(self.read_property(
                host,
                roots,
                items,
                PropertyKeyRef::String("length")
            )));
            let length = script!(self.coerce_number(host, roots, &length))
                .trunc()
                .max(0.0);
            if length > MAX_LENGTH as f64 {
                return Ok(Err(self.make_range_error("invalid array length")?));
            }
            for index in 0..length as usize {
                let roots = &roots.with(&values);
                let mut value = script!(completion(self.read_property(
                    host,
                    roots,
                    items,
                    PropertyKeyRef::String(&index.to_string())
                )));
                if mapping {
                    value = script!(completion(self.invoke(
                        host,
                        roots,
                        mapper,
                        vec![value, number(index)],
                        arg(args, 2)
                    )));
                }
                values.push(value);
            }
        }
        Ok(Ok(Value::Object(self.heap.alloc_array(values))))
    }
}

struct FlattenRoots<'a>(&'a [(ObjectId, std::ops::Range<usize>, f64)]);

impl crate::gc::TraceRoots for FlattenRoots<'_> {
    fn trace(&self, tracer: &mut crate::gc::Tracer<'_>) -> Result<(), VmError> {
        for (id, _, _) in self.0 {
            crate::gc::TraceRoots::trace(id, tracer)?;
        }
        Ok(())
    }
}

struct ArrayAccess<'a> {
    vm: &'a mut Vm,
    host: &'a mut dyn Host,
    roots: &'a ActiveRoots<'a>,
    id: ObjectId,
    // Snapshot once: callbacks and property hooks can mutate the live array.
    len: usize,
}

impl ArrayAccess<'_> {
    fn with_roots<T>(
        &mut self,
        view: &dyn crate::gc::TraceRoots,
        f: impl FnOnce(&mut ArrayAccess<'_>) -> T,
    ) -> T {
        let roots = &self.roots.with(view);
        f(&mut ArrayAccess {
            vm: self.vm,
            host: self.host,
            roots,
            id: self.id,
            len: self.len,
        })
    }
    fn get(&mut self, id: ObjectId, index: usize) -> Completion<Value> {
        completion(self.vm.read_property(
            self.host,
            self.roots,
            &Value::Object(id),
            PropertyKeyRef::String(&index.to_string()),
        ))
    }

    fn present(&mut self, id: ObjectId, index: usize) -> Completion<bool> {
        completion(self.vm.has_property_key(
            self.host,
            self.roots,
            id,
            PropertyKeyRef::String(&index.to_string()),
        ))
        .map(|result| result.map(|value| value.truthy()))
    }

    fn integer(&mut self, value: &Value) -> Completion<f64> {
        let n = script!(self.vm.coerce_number(self.host, self.roots, value));
        Ok(Ok(if n.is_nan() { 0.0 } else { n.trunc() }))
    }

    fn set(&mut self, key: String, value: Value) -> Completion<Value> {
        completion(self.vm.set_property(
            self.host,
            self.roots,
            self.id,
            &Value::Object(self.id),
            PropertyKey::String(key.into()),
            value,
            true,
        ))
    }

    fn delete(&mut self, index: usize) -> Completion<()> {
        if self
            .vm
            .heap
            .delete_own(self.id, PropertyKeyRef::String(&index.to_string()))?
        {
            Ok(Ok(()))
        } else {
            Ok(Err(self.vm.type_error("cannot delete array element")?))
        }
    }

    fn define(&mut self, id: ObjectId, index: usize, value: Value) -> Result<(), VmError> {
        self.vm.heap.define_data(
            id,
            PropertyKey::String(index.to_string().into()),
            value,
            Attributes::ORDINARY,
        )
    }

    fn move_index(&mut self, from: usize, to: usize) -> Completion<()> {
        if script!(self.present(self.id, from)) {
            let value = script!(self.get(self.id, from));
            script!(self.set(to.to_string(), value));
        } else {
            script!(self.delete(to));
        }
        Ok(Ok(()))
    }

    fn mutate(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let len = self.len;
        let (start, count, inserts) = match name {
            "push" => (len, 0, args),
            "unshift" => (0, 0, args),
            "pop" => (len.saturating_sub(1), usize::from(len != 0), &[][..]),
            "shift" => (0, usize::from(len != 0), &[][..]),
            "splice" => {
                let start = relative(script!(self.integer(arg(args, 0))), len);
                let count = match args.len() {
                    0 => 0,
                    1 => len - start,
                    _ => script!(self.integer(&args[1])).clamp(0.0, (len - start) as f64) as usize,
                };
                (start, count, args.get(2..).unwrap_or_default())
            }
            _ => unreachable!(),
        };
        let Some(new_len) = (len - count)
            .checked_add(inserts.len())
            .filter(|len| *len <= MAX_LENGTH)
        else {
            return Ok(Err(self.vm.make_range_error("invalid array length")?));
        };
        let value = match name {
            "pop" | "shift" if count != 0 => script!(self.get(self.id, start)),
            "pop" | "shift" => Value::Undefined,
            "splice" => Value::Object(script!(self.copy_range(start, count))),
            _ => number(new_len),
        };
        script!(self.with_roots(&value, |array| -> Completion<()> {
            if inserts.len() < count {
                for index in start + count..len {
                    script!(array.move_index(index, index - count + inserts.len()));
                }
                for index in (new_len..len).rev() {
                    script!(array.delete(index));
                }
            } else if inserts.len() > count {
                for index in (start + count..len).rev() {
                    script!(array.move_index(index, index - count + inserts.len()));
                }
            }
            for (index, value) in inserts.iter().enumerate() {
                script!(array.set((start + index).to_string(), value.clone()));
            }
            script!(array.set("length".to_owned(), number(new_len)));
            Ok(Ok(()))
        }));
        Ok(Ok(value))
    }

    fn copy_range(&mut self, start: usize, count: usize) -> Completion<ObjectId> {
        let result = self.vm.sparse_array(count)?;
        for i in 0..count {
            if script!(self.with_roots(&result, |array| array.present(array.id, start + i))) {
                let value =
                    script!(self.with_roots(&result, |array| array.get(array.id, start + i)));
                self.define(result, i, value)?;
            }
        }
        Ok(Ok(result))
    }

    fn range(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let offset = usize::from(name == "fill");
        let start = relative(script!(self.integer(arg(args, offset))), self.len);
        let end = match arg(args, offset + 1) {
            Value::Undefined => self.len,
            value => relative(script!(self.integer(value)), self.len),
        };
        if name == "fill" {
            for index in start..end {
                script!(self.set(index.to_string(), arg(args, 0).clone()));
            }
            Ok(Ok(Value::Object(self.id)))
        } else {
            Ok(Ok(Value::Object(script!(
                self.copy_range(start, end.saturating_sub(start))
            ))))
        }
    }

    fn search(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let includes = name == "includes";
        let miss = if includes {
            Value::Bool(false)
        } else {
            Value::Number(-1.0)
        };
        if self.len == 0 {
            return Ok(Ok(miss));
        }
        let last = name == "lastIndexOf";
        let from = if last && args.len() < 2 {
            (self.len - 1) as f64
        } else {
            script!(self.integer(arg(args, 1)))
        };
        let from = if from < 0.0 {
            self.len as f64 + from
        } else {
            from
        };
        let (start, count) = if last {
            if from < 0.0 {
                return Ok(Ok(miss));
            }
            let start = (from as usize).min(self.len - 1);
            (start, start + 1)
        } else {
            let start = (from as usize).min(self.len);
            (start, self.len - start)
        };
        let needle = arg(args, 0);
        for offset in 0..count {
            let index = if last { start - offset } else { start + offset };
            if includes || script!(self.present(self.id, index)) {
                let value = script!(self.get(self.id, index));
                let both_nan = matches!((&value, needle), (Value::Number(a), Value::Number(b)) if a.is_nan() && b.is_nan());
                if strict_equal(&value, needle) || includes && both_nan {
                    return Ok(Ok(if includes {
                        Value::Bool(true)
                    } else {
                        number(index)
                    }));
                }
            }
        }
        Ok(Ok(miss))
    }

    fn callback(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let mapper = arg(args, 0);
        if !self.vm.is_callable(mapper) {
            return Ok(Err(self.vm.type_error("Array callback is not callable")?));
        }
        let result = match name {
            "map" => Some(self.vm.sparse_array(self.len)?),
            "filter" | "flatMap" => Some(self.vm.sparse_array(0)?),
            _ => None,
        };
        let mut output = 0;
        let mut accumulator = args.get(1).cloned();
        let visit_holes = matches!(name, "find" | "findIndex");
        let source = Value::Object(self.id);
        for index in 0..self.len {
            let view = (result, &accumulator);
            if !visit_holes
                && !script!(self.with_roots(&view, |array| array.present(array.id, index)))
            {
                continue;
            }
            let value = script!(self.with_roots(&view, |array| array.get(array.id, index)));
            if name == "reduce" {
                accumulator = Some(match accumulator {
                    Some(acc) => script!(completion(self.vm.invoke(
                        self.host,
                        self.roots,
                        mapper,
                        vec![acc, value, number(index), source.clone()],
                        &Value::Undefined
                    ))),
                    None => value,
                });
                continue;
            }
            let preserve_value = matches!(name, "filter" | "find");
            let saved = preserve_value.then(|| value.clone());
            let callback_view = (result, &saved);
            let mapped = script!(completion(self.vm.invoke(
                self.host,
                &self.roots.with(&callback_view),
                mapper,
                vec![value, number(index), source.clone()],
                arg(args, 1)
            )));
            match name {
                "map" => self.define(result.unwrap(), index, mapped)?,
                "filter" if mapped.truthy() => {
                    self.define(result.unwrap(), output, saved.unwrap())?;
                    output += 1;
                }
                "flatMap" => script!(self.flatten_value(result.unwrap(), &mut output, mapped, 1.0)),
                "some" if mapped.truthy() => return Ok(Ok(Value::Bool(true))),
                "every" if !mapped.truthy() => return Ok(Ok(Value::Bool(false))),
                "find" if mapped.truthy() => return Ok(Ok(saved.unwrap())),
                "findIndex" if mapped.truthy() => return Ok(Ok(number(index))),
                _ => {}
            }
        }
        Ok(match name {
            "reduce" => match accumulator {
                Some(value) => Ok(value),
                None => Err(self
                    .vm
                    .type_error("reduce of empty array without initial value")?),
            },
            "some" => Ok(Value::Bool(false)),
            "every" => Ok(Value::Bool(true)),
            "findIndex" => Ok(Value::Number(-1.0)),
            _ => Ok(result.map_or(Value::Undefined, Value::Object)),
        })
    }

    fn combine(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let result = self.vm.sparse_array(0)?;
        let mut output = 0;
        if name == "concat" {
            for value in std::iter::once(&Value::Object(self.id)).chain(args) {
                if let Some(len) = self.vm.array_length(value)? {
                    let Value::Object(id) = *value else {
                        unreachable!()
                    };
                    if len > MAX_LENGTH - output {
                        return Ok(Err(self.vm.make_range_error("invalid array length")?));
                    }
                    for index in 0..len {
                        if script!(self.with_roots(&result, |array| array.present(id, index))) {
                            let item =
                                script!(self.with_roots(&result, |array| array.get(id, index)));
                            self.define(result, output + index, item)?;
                        }
                    }
                    output += len;
                } else {
                    script!(self.flatten_value(result, &mut output, value.clone(), 0.0));
                }
            }
            let Object::Array(array) = self.vm.heap.get_mut(result)? else {
                unreachable!()
            };
            array.length = output;
        } else {
            let depth = match arg(args, 0) {
                Value::Undefined => 1.0,
                value => script!(self.with_roots(&result, |array| array.integer(value))),
            };
            for index in 0..self.len {
                if script!(self.with_roots(&result, |array| array.present(array.id, index))) {
                    let item =
                        script!(self.with_roots(&result, |array| array.get(array.id, index)));
                    script!(self.flatten_value(result, &mut output, item, depth));
                }
            }
        }
        Ok(Ok(Value::Object(result)))
    }

    fn flatten_value(
        &mut self,
        result: ObjectId,
        output: &mut usize,
        mut value: Value,
        mut depth: f64,
    ) -> Completion<()> {
        // Explicit DFS stack: nested arrays need not fit on the native stack.
        let mut stack = Vec::<(ObjectId, std::ops::Range<usize>, f64)>::new();
        loop {
            if depth > 0.0
                && let Some(len) = self.vm.array_length(&value)?
            {
                let Value::Object(id) = value else {
                    unreachable!()
                };
                if depth - 1.0 == depth && stack.iter().any(|(ancestor, _, _)| *ancestor == id) {
                    return Ok(Err(self
                        .vm
                        .type_error("cannot flatten a cyclic array at this depth")?));
                }
                stack.push((id, 0..len, depth - 1.0));
            } else {
                if *output == MAX_LENGTH {
                    return Ok(Err(self.vm.make_range_error("invalid array length")?));
                }
                self.define(result, *output, value)?;
                *output += 1;
            }
            loop {
                let Some((id, indices, inner_depth)) = stack.last_mut() else {
                    return Ok(Ok(()));
                };
                if let Some(index) = indices.next() {
                    let (id, inner_depth) = (*id, *inner_depth);
                    let view = (result, FlattenRoots(&stack));
                    if script!(self.with_roots(&view, |array| array.present(id, index))) {
                        value = script!(self.with_roots(&view, |array| array.get(id, index)));
                        depth = inner_depth;
                        break;
                    }
                } else {
                    stack.pop();
                }
            }
        }
    }
}
