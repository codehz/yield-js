//! Array builtins use ordinary property operations for observable reads and writes.
//! New results define own data properties, bypassing inherited setters.

use crate::error::VmError;
use crate::heap::{Attributes, ITERATOR_SYMBOL, Object};
use crate::host::Host;
use crate::ops::strict_equal;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

const MAX_LENGTH: usize = u32::MAX as usize;
const STATICS: &[&str] = &["isArray", "from", "of"];
const METHODS: &[&str] = &[
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "slice",
    "fill",
    "map",
    "filter",
    "reduce",
    "forEach",
    "some",
    "every",
    "flat",
    "indexOf",
    "lastIndexOf",
    "includes",
    "concat",
    "find",
    "findIndex",
    "flatMap",
];

pub(crate) fn is_array_operation(name: &str) -> bool {
    STATICS.contains(&name) || METHODS.contains(&name)
}

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
    pub(crate) fn init_array_constructor(&mut self, prototype: ObjectId) -> ObjectId {
        let constructor = self.heap.alloc_builtin("Array", None);
        self.heap
            .define_data(
                constructor,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("Array stores its prototype");
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(constructor),
                Attributes::METHOD,
            )
            .expect("Array.prototype stores its constructor");
        for (target, names) in [(constructor, STATICS), (prototype, METHODS)] {
            for &name in names {
                let id = self.heap.alloc_builtin("Array", Some(name));
                self.heap
                    .define_data(target, name, Value::Object(id), Attributes::METHOD)
                    .expect("Array stores its builtin methods");
            }
        }
        constructor
    }

    pub(crate) fn construct_array(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
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

    pub(crate) fn array_operation(
        &mut self,
        host: &mut dyn Host,
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
            "from" => return invocation(self.array_from(host, &args)),
            _ => {}
        }
        let Some(len) = self.array_length(receiver)? else {
            return self.throw_type_error("Array method requires an array receiver");
        };
        let Value::Object(id) = *receiver else {
            unreachable!()
        };
        let mut array = ArrayAccess {
            vm: self,
            host,
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

    fn array_from(&mut self, host: &mut dyn Host, args: &[Value]) -> Completion<Value> {
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
                items,
                PropertyKeyRef::Symbol(ITERATOR_SYMBOL)
            ))),
            _ => Value::Undefined,
        };
        let mut values = Vec::new();
        if !matches!(method, Value::Null | Value::Undefined) {
            let (iter, next) = script!(self.get_iterator_from_method(host, items, &method));
            while let Some(mut value) = script!(self.iterator_step(host, &iter, &next)) {
                if values.len() == MAX_LENGTH {
                    self.close_iterator(host, &iter)?;
                    return Ok(Err(self.make_range_error("invalid array length")?));
                }
                if mapping {
                    value = match self.invoke(
                        host,
                        mapper,
                        vec![value, number(values.len())],
                        arg(args, 2),
                    )? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => {
                            // IteratorClose preserves the original throw.
                            self.close_iterator(host, &iter)?;
                            return Ok(Err(error));
                        }
                    };
                }
                values.push(value);
            }
        } else if matches!(items, Value::Object(_)) {
            let length = script!(completion(self.read_property(
                host,
                items,
                PropertyKeyRef::String("length")
            )));
            let length = script!(self.coerce_number(host, &length)).trunc().max(0.0);
            if length > MAX_LENGTH as f64 {
                return Ok(Err(self.make_range_error("invalid array length")?));
            }
            for index in 0..length as usize {
                let mut value = script!(completion(self.read_property(
                    host,
                    items,
                    PropertyKeyRef::String(&index.to_string())
                )));
                if mapping {
                    value = script!(completion(self.invoke(
                        host,
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

struct ArrayAccess<'a> {
    vm: &'a mut Vm,
    host: &'a mut dyn Host,
    id: ObjectId,
    // Snapshot once: callbacks and property hooks can mutate the live array.
    len: usize,
}

impl ArrayAccess<'_> {
    fn get(&mut self, id: ObjectId, index: usize) -> Completion<Value> {
        completion(self.vm.read_property(
            self.host,
            &Value::Object(id),
            PropertyKeyRef::String(&index.to_string()),
        ))
    }

    fn present(&mut self, id: ObjectId, index: usize) -> Completion<bool> {
        completion(self.vm.has_property_key(
            self.host,
            id,
            PropertyKeyRef::String(&index.to_string()),
        ))
        .map(|result| result.map(|value| value.truthy()))
    }

    fn integer(&mut self, value: &Value) -> Completion<f64> {
        let n = script!(self.vm.coerce_number(self.host, value));
        Ok(Ok(if n.is_nan() { 0.0 } else { n.trunc() }))
    }

    fn set(&mut self, key: String, value: Value) -> Completion<Value> {
        completion(self.vm.set_property(
            self.host,
            self.id,
            &Value::Object(self.id),
            PropertyKey::String(key),
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
            PropertyKey::String(index.to_string()),
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
        if inserts.len() < count {
            for index in start + count..len {
                script!(self.move_index(index, index - count + inserts.len()));
            }
            for index in (new_len..len).rev() {
                script!(self.delete(index));
            }
        } else if inserts.len() > count {
            for index in (start + count..len).rev() {
                script!(self.move_index(index, index - count + inserts.len()));
            }
        }
        for (index, value) in inserts.iter().enumerate() {
            script!(self.set((start + index).to_string(), value.clone()));
        }
        script!(self.set("length".to_owned(), number(new_len)));
        Ok(Ok(value))
    }

    fn copy_range(&mut self, start: usize, count: usize) -> Completion<ObjectId> {
        let result = self.vm.sparse_array(count)?;
        for i in 0..count {
            if script!(self.present(self.id, start + i)) {
                let value = script!(self.get(self.id, start + i));
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
            if !visit_holes && !script!(self.present(self.id, index)) {
                continue;
            }
            let value = script!(self.get(self.id, index));
            if name == "reduce" {
                accumulator = Some(match accumulator {
                    Some(acc) => script!(completion(self.vm.invoke(
                        self.host,
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
            let mapped = script!(completion(self.vm.invoke(
                self.host,
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
                        if script!(self.present(id, index)) {
                            let item = script!(self.get(id, index));
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
                value => script!(self.integer(value)),
            };
            for index in 0..self.len {
                if script!(self.present(self.id, index)) {
                    let item = script!(self.get(self.id, index));
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
                    if script!(self.present(*id, index)) {
                        value = script!(self.get(*id, index));
                        depth = *inner_depth;
                        break;
                    }
                } else {
                    stack.pop();
                }
            }
        }
    }
}
