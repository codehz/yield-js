//! Array builtins use ordinary property operations for observable reads and writes.
//! New results define own data properties, bypassing inherited setters.

use crate::builtin::globals::Owner;
use crate::builtin::{RootKind, RootSpec, define_builtin_data, link_constructor};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ArrayIterationKind, Attributes, DescriptorUpdate, IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_SYMBOL,
    Object, SPECIES_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops::strict_equal;
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const MAX_LENGTH: u64 = u32::MAX as u64;
const MAX_SAFE_LENGTH: u64 = (1 << 53) - 1;

#[yield_binding_macros::builtin_global(name = "Array", root = "array", kind = "constructor")]
mod members {
    use super::{ArrayIterationKind, Invocation, SPECIES_SYMBOL};

    static_calls! {
        get symbol(SPECIES_SYMBOL) species: 0 => |_, _, _, _, receiver| {
            Ok(Invocation::Value(receiver.clone()))
        };
        isArray: 1 | from: 1 | of: 0 as name => |vm, host, roots, args, receiver| {
            vm.array_operation(host, roots, name, receiver, args)
        };
    }

    prototype_calls! {
        push: 1 | pop: 0 | shift: 0 | unshift: 1 | splice: 2 | slice: 2 | fill: 1 | map: 1 | filter: 1 | reduce: 1 | forEach: 1
            | some: 1 | every: 1 | flat: 0 | indexOf: 1 | lastIndexOf: 1 | includes: 1 | concat: 1 | find: 1 | findIndex: 1
            | flatMap: 1 | at: 1 | findLast: 1 | findLastIndex: 1 | reduceRight: 1 | reverse: 0 | copyWithin: 2
            | sort: 1 | toSorted: 1 | toReversed: 0 | with: 2 | toSpliced: 2 as name => |vm, host, roots, args, receiver| {
                vm.array_operation(host, roots, name, receiver, args)
            };
        keys: 0 => |vm, _, _, _, receiver| vm.array_iterator(receiver, ArrayIterationKind::Keys);
        entries: 0 => |vm, _, _, _, receiver| vm.array_iterator(receiver, ArrayIterationKind::Entries);
        join: 1 => |vm, host, roots, args, receiver| vm.array_join(host, roots, receiver, &args);
        toString: 0 => |vm, host, roots, _, receiver| vm.array_to_string(host, roots, receiver);
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
        Some(super::iterator::SYNC_ROOT),
    ),
];

pub(crate) const ARRAY_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        ARRAY_CONSTRUCTOR,
        1,
        |vm, _, _, args, _| vm.construct_array(args),
        |vm, _, _, args, prototype| {
            let result = vm.construct_array(args)?;
            vm.with_result_prototype(result, prototype)
        },
    ),
    IntrinsicDef::call(ARRAY_VALUES, 0, |vm, _, _, _, receiver| {
        vm.array_iterator(receiver, ArrayIterationKind::Values)
    }),
    IntrinsicDef::call(ARRAY_ITERATOR_NEXT, 0, |vm, host, roots, _, receiver| {
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

fn number(index: u64) -> Value {
    Value::number(index as f64)
}

fn arg(args: &[Value], index: usize) -> &Value {
    static UNDEFINED: Value = Value::UNDEFINED;
    args.get(index).unwrap_or(&UNDEFINED)
}

fn relative(index: f64, len: u64) -> u64 {
    let len = len as f64;
    (if index < 0.0 { len + index } else { index }).clamp(0.0, len) as u64
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
            Value::object(values),
            Attributes::METHOD,
        );
        self.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                Value::object(values),
                Attributes::METHOD,
            )
            .expect("Array iterator alias");
        let next = self.alloc_intrinsic(ARRAY_ITERATOR_NEXT);
        self.heap
            .define_data(iterator, "next", Value::object(next), Attributes::METHOD)
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
        let length = match args.as_slice() {
            [arg] => match arg.view() {
                ValueRef::Number(length) => Some(length),
                _ => None,
            },
            _ => None,
        };
        let id = match length {
            Some(length) => {
                if !(0.0..=MAX_LENGTH as f64).contains(&length) || length.fract() != 0.0 {
                    return self.throw_range_error("invalid array length");
                }
                self.sparse_array(length as u64)?
            }
            None => self.heap.alloc_array(args),
        };
        Ok(Invocation::Value(Value::object(id)))
    }

    fn sparse_array(&mut self, length: u64) -> Result<ObjectId, VmError> {
        let id = self.heap.alloc_array(Vec::new());
        let Object::Array(array) = self.heap.get_mut(id)? else {
            unreachable!()
        };
        array.length = length as usize;
        Ok(id)
    }

    fn array_create(&mut self, length: u64) -> Completion<ObjectId> {
        if length > MAX_LENGTH {
            return Ok(Err(self.make_range_error("invalid array length")?));
        }
        Ok(Ok(self.sparse_array(length)?))
    }

    pub(crate) fn array_receiver(&mut self, value: &Value) -> Completion<ObjectId> {
        if matches!(value.view(), ValueRef::Null | ValueRef::Undefined) {
            return Ok(Err(
                self.type_error("Array method requires a non-nullish receiver")?
            ));
        }
        Ok(Ok(self.coercible_object(value)))
    }

    pub(crate) fn length_of_array_like(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Completion<u64> {
        if let Object::Array(array) = self.heap.get(id)? {
            return Ok(Ok(array.length as u64));
        }
        let roots = &roots.with(&id);
        let length = script!(completion(self.read_property(
            host,
            roots,
            &Value::object(id),
            PropertyKeyRef::String("length")
        )));
        let n = script!(self.coerce_number(host, roots, &length));
        Ok(Ok(n.trunc().max(0.0).min(MAX_SAFE_LENGTH as f64) as u64))
    }

    pub(crate) fn read_array_like_index(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        index: u64,
    ) -> Result<Invocation, VmError> {
        if index < MAX_LENGTH && matches!(self.heap.get(id)?, Object::Array(_)) {
            return self.read_array_index(host, roots, id, index as usize);
        }
        self.read_property(
            host,
            roots,
            &Value::object(id),
            PropertyKeyRef::String(&index.to_string()),
        )
    }

    fn is_array(&self, value: &Value) -> Result<bool, VmError> {
        Ok(match value.view() {
            ValueRef::Object(id) => matches!(
                self.heap.get(self.heap.proxy_target(id)?)?,
                Object::Array(_)
            ),
            _ => false,
        })
    }

    fn construct_array_result(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        args: Vec<Value>,
    ) -> Completion<ArrayResult> {
        let result = script!(completion(self.construct_with(
            host,
            roots,
            constructor,
            Arguments::Values(&args),
            constructor
        )));
        let ValueRef::Object(id) = result.view() else {
            unreachable!("construct returns an object")
        };
        Ok(Ok(ArrayResult { id, fresh: false }))
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
                return invocation(self.array_of(host, roots, receiver, args));
            }
            "isArray" => {
                return Ok(Invocation::Value(Value::boolean(
                    self.is_array(arg(&args, 0))?,
                )));
            }
            "from" => return invocation(self.array_from(host, roots, receiver, &args)),
            _ => {}
        }
        invocation(self.array_prototype_operation(host, roots, name, receiver, &args))
    }

    fn array_prototype_operation(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        name: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Completion<Value> {
        if matches!(name, "sort" | "toSorted")
            && !matches!(arg(args, 0).view(), ValueRef::Undefined)
            && !self.is_callable(arg(args, 0))
        {
            return Ok(Err(self.type_error("Array comparator is not callable")?));
        }
        let id = script!(self.array_receiver(receiver));
        let view = (id, args);
        let roots = &roots.with(&view);
        let len = if name == "concat" {
            0
        } else {
            script!(self.length_of_array_like(host, roots, id))
        };
        let mut array = ArrayAccess {
            vm: self,
            host,
            roots,
            id,
            len,
        };
        match name {
            "push" | "pop" | "shift" | "unshift" | "splice" => array.mutate(name, args),
            "at" | "reverse" | "copyWithin" => array.reindex(name, args),
            "sort" | "toSorted" => array.sort(name == "toSorted", arg(args, 0)),
            "toReversed" | "with" | "toSpliced" => array.copying(name, args),
            "slice" | "fill" => array.range(name, args),
            "indexOf" | "lastIndexOf" | "includes" => array.search(name, args),
            "concat" | "flat" => array.combine(name, args),
            _ => array.callback(name, args),
        }
    }

    fn array_of(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        args: Vec<Value>,
    ) -> Completion<Value> {
        if !self.is_constructor(constructor)?
            || matches!(constructor.view(), ValueRef::Object(id) if id == self.builtins.get("array"))
        {
            return Ok(Ok(Value::object(self.heap.alloc_array(args))));
        }
        let roots = &roots.with(&args);
        let length = args.len() as u64;
        let result =
            script!(self.construct_array_result(host, roots, constructor, vec![number(length)]));
        let roots = &roots.with(&result);
        let mut access = ArrayAccess {
            vm: self,
            host,
            roots,
            id: result.id,
            len: length,
        };
        for (index, value) in args.iter().enumerate() {
            script!(access.define(result, index as u64, value.clone()));
        }
        script!(access.set("length".into(), number(length)));
        Ok(Ok(Value::object(result.id)))
    }

    fn array_from(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        args: &[Value],
    ) -> Completion<Value> {
        let view = (constructor, args);
        let roots = &roots.with(&view);
        let items = arg(args, 0);
        let mapper = arg(args, 1);
        let mapping = !matches!(mapper.view(), ValueRef::Undefined);
        if mapping && !self.is_callable(mapper) {
            return Ok(Err(self.type_error("Array.from mapper is not callable")?));
        }
        if matches!(items.view(), ValueRef::Null | ValueRef::Undefined) {
            return Ok(Err(self.type_error("Array.from requires items")?));
        }
        let method = script!(completion(self.read_property(
            host,
            roots,
            items,
            PropertyKeyRef::Symbol(ITERATOR_SYMBOL)
        )));
        let iterable = !matches!(method.view(), ValueRef::Null | ValueRef::Undefined);
        if iterable && !self.is_callable(&method) {
            return Ok(Err(
                self.type_error("Array.from iterator method is not callable")?
            ));
        }
        let roots = &roots.with(&method);
        let source = if iterable {
            None
        } else {
            Some(self.coercible_object(items))
        };
        let roots = &roots.with(&source);
        let length = if let Some(source) = source {
            script!(self.length_of_array_like(host, roots, source))
        } else {
            0
        };
        let result = if self.is_constructor(constructor)?
            && !matches!(constructor.view(), ValueRef::Object(id) if id == self.builtins.get("array"))
        {
            let args = if iterable {
                Vec::new()
            } else {
                vec![number(length)]
            };
            script!(self.construct_array_result(host, roots, constructor, args))
        } else {
            ArrayResult {
                id: script!(self.array_create(length)),
                fresh: true,
            }
        };
        let roots = &roots.with(&result);
        let mut access = ArrayAccess {
            vm: self,
            host,
            roots,
            id: result.id,
            len: length,
        };
        let map_and_define = |access: &mut ArrayAccess<'_>, index, mut value| -> Completion<()> {
            if mapping {
                value = script!(completion(access.vm.invoke(
                    access.host,
                    access.roots,
                    mapper,
                    Arguments::Values(&[value, number(index)]),
                    arg(args, 2)
                )));
            }
            access.define(result, index, value)
        };
        let length = if iterable {
            let (iter, next) = script!(access.vm.get_iterator_from_method(
                access.host,
                roots,
                items,
                &method
            ));
            let view = (&iter, &next);
            script!(access.with_roots(&view, |access| -> Completion<u64> {
                let mut index = 0;
                loop {
                    let failure = if index == MAX_SAFE_LENGTH {
                        access
                            .vm
                            .type_error("Array.from exceeds the safe integer limit")?
                    } else {
                        let Some(value) = script!(access.vm.iterator_step(
                            access.host,
                            access.roots,
                            &iter,
                            &next
                        )) else {
                            return Ok(Ok(index));
                        };
                        match map_and_define(access, index, value)? {
                            Ok(()) => {
                                index += 1;
                                continue;
                            }
                            Err(error) => error,
                        }
                    };
                    access
                        .vm
                        .close_iterator(access.host, &access.roots.with(&failure), &iter)?;
                    return Ok(Err(failure));
                }
            }))
        } else if let Some(source) = source {
            for index in 0..length {
                let value = script!(access.get(source, index));
                script!(map_and_define(&mut access, index, value));
            }
            length
        } else {
            unreachable!("array-like input was boxed")
        };
        script!(access.set("length".into(), number(length)));
        Ok(Ok(Value::object(result.id)))
    }
}

#[derive(Clone, Copy)]
struct ArrayResult {
    id: ObjectId,
    fresh: bool,
}

impl crate::gc::TraceRoots for ArrayResult {
    fn trace(&self, tracer: &mut crate::gc::Tracer<'_>) -> Result<(), VmError> {
        crate::gc::TraceRoots::trace(&self.id, tracer)
    }
}

struct FlattenRoots<'a>(&'a [(ObjectId, std::ops::Range<u64>, f64)]);

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
    len: u64,
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
    fn get(&mut self, id: ObjectId, index: u64) -> Completion<Value> {
        completion(
            self.vm
                .read_array_like_index(self.host, self.roots, id, index),
        )
    }

    fn present(&mut self, id: ObjectId, index: u64) -> Completion<bool> {
        if index < MAX_LENGTH
            && let Object::Array(array) = self.vm.heap.get(id)?
            && array.own_index(index as usize).is_some()
        {
            return Ok(Ok(true));
        }
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
        self.set_on(self.id, key, value)
    }

    fn set_on(&mut self, id: ObjectId, key: String, value: Value) -> Completion<Value> {
        completion(self.vm.set_property(
            self.host,
            self.roots,
            id,
            &Value::object(id),
            PropertyKey::String(key.into()),
            value,
            true,
        ))
    }

    fn delete(&mut self, index: u64) -> Completion<()> {
        let deleted = script!(completion(self.vm.delete_property(
            self.host,
            self.roots,
            &Value::object(self.id),
            &Value::string(index.to_string().into())
        )));
        if deleted.truthy() {
            Ok(Ok(()))
        } else {
            Ok(Err(self.vm.type_error("cannot delete array element")?))
        }
    }

    fn define(&mut self, result: ArrayResult, index: u64, value: Value) -> Completion<()> {
        let key = PropertyKey::String(index.to_string().into());
        if result.fresh {
            self.vm
                .heap
                .define_data(result.id, key, value, Attributes::ORDINARY)?;
        } else if !script!(self.vm.try_apply_property_update(
            self.host,
            self.roots,
            result.id,
            key,
            DescriptorUpdate {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..DescriptorUpdate::default()
            },
        )) {
            return Ok(Err(self
                .vm
                .type_error("cannot define array result element")?));
        }
        Ok(Ok(()))
    }

    fn species_create(&mut self, length: u64) -> Completion<ArrayResult> {
        let source = Value::object(self.id);
        if self.vm.is_array(&source)? {
            let mut constructor = script!(completion(self.vm.read_property(
                self.host,
                self.roots,
                &source,
                PropertyKeyRef::String("constructor")
            )));
            if matches!(constructor.view(), ValueRef::Object(_)) {
                constructor = script!(completion(self.vm.read_property(
                    self.host,
                    self.roots,
                    &constructor,
                    PropertyKeyRef::Symbol(SPECIES_SYMBOL)
                )));
                if matches!(constructor.view(), ValueRef::Null) {
                    constructor = Value::UNDEFINED;
                }
            }
            if !matches!(constructor.view(), ValueRef::Undefined)
                && !matches!(constructor.view(), ValueRef::Object(id) if id == self.vm.builtins.get("array"))
            {
                return self.vm.construct_array_result(
                    self.host,
                    self.roots,
                    &constructor,
                    vec![number(length)],
                );
            }
        }
        Ok(Ok(ArrayResult {
            id: script!(self.vm.array_create(length)),
            fresh: true,
        }))
    }

    fn move_index(&mut self, from: u64, to: u64) -> Completion<()> {
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
            "pop" => (len.saturating_sub(1), u64::from(len != 0), &[][..]),
            "shift" => (0, u64::from(len != 0), &[][..]),
            "splice" => {
                let start = relative(script!(self.integer(arg(args, 0))), len);
                let count = match args.len() {
                    0 => 0,
                    1 => len - start,
                    _ => script!(self.integer(&args[1])).clamp(0.0, (len - start) as f64) as u64,
                };
                (start, count, args.get(2..).unwrap_or_default())
            }
            _ => unreachable!(),
        };
        let insert_count = inserts.len() as u64;
        let new_len = len - count + insert_count;
        if new_len > MAX_SAFE_LENGTH {
            return Ok(Err(self
                .vm
                .type_error("array-like length exceeds the safe integer limit")?));
        }
        let value = match name {
            "pop" | "shift" if count != 0 => script!(self.get(self.id, start)),
            "pop" | "shift" => Value::UNDEFINED,
            "splice" => Value::object(script!(self.copy_range(start, count))),
            _ => number(new_len),
        };
        script!(self.with_roots(&value, |array| -> Completion<()> {
            if insert_count < count {
                for index in start + count..len {
                    script!(array.move_index(index, index - count + insert_count));
                }
                for index in (new_len..len).rev() {
                    script!(array.delete(index));
                }
            } else if insert_count > count {
                for index in (start + count..len).rev() {
                    script!(array.move_index(index, index - count + insert_count));
                }
            }
            for (index, value) in inserts.iter().enumerate() {
                script!(array.set((start + index as u64).to_string(), value.clone()));
            }
            script!(array.set("length".to_owned(), number(new_len)));
            Ok(Ok(()))
        }));
        Ok(Ok(value))
    }

    fn copy_range(&mut self, start: u64, count: u64) -> Completion<ObjectId> {
        let result = script!(self.species_create(count));
        for i in 0..count {
            if script!(self.with_roots(&result, |array| array.present(array.id, start + i))) {
                let value =
                    script!(self.with_roots(&result, |array| array.get(array.id, start + i)));
                script!(self.define(result, i, value));
            }
        }
        script!(self.with_roots(&result, |array| array.set_on(
            result.id,
            "length".into(),
            number(count)
        )));
        Ok(Ok(result.id))
    }

    fn range(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let offset = usize::from(name == "fill");
        let start = relative(script!(self.integer(arg(args, offset))), self.len);
        let end = match arg(args, offset + 1) {
            value if matches!(value.view(), ValueRef::Undefined) => self.len,
            value => relative(script!(self.integer(value)), self.len),
        };
        if name == "fill" {
            for index in start..end {
                script!(self.set(index.to_string(), arg(args, 0).clone()));
            }
            Ok(Ok(Value::object(self.id)))
        } else {
            Ok(Ok(Value::object(script!(
                self.copy_range(start, end.saturating_sub(start))
            ))))
        }
    }

    fn reindex(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let len = self.len;
        match name {
            "at" => {
                let index = script!(self.integer(arg(args, 0)));
                let index = if index < 0.0 {
                    len as f64 + index
                } else {
                    index
                };
                return if index < 0.0 || index >= len as f64 {
                    Ok(Ok(Value::UNDEFINED))
                } else {
                    self.get(self.id, index as u64)
                };
            }
            "copyWithin" => {
                let to = relative(script!(self.integer(arg(args, 0))), len);
                let from = relative(script!(self.integer(arg(args, 1))), len);
                let end = match arg(args, 2) {
                    value if matches!(value.view(), ValueRef::Undefined) => len,
                    value => relative(script!(self.integer(value)), len),
                };
                let count = end.saturating_sub(from).min(len - to);
                let backwards = from < to && to < from + count;
                for offset in 0..count {
                    let offset = if backwards {
                        count - 1 - offset
                    } else {
                        offset
                    };
                    script!(self.move_index(from + offset, to + offset));
                }
            }
            "reverse" => {
                for lower in 0..len / 2 {
                    let upper = len - lower - 1;
                    let low = if script!(self.present(self.id, lower)) {
                        Some(script!(self.get(self.id, lower)))
                    } else {
                        None
                    };
                    script!(self.with_roots(&low, |array| -> Completion<()> {
                        let high = if script!(array.present(array.id, upper)) {
                            Some(script!(array.get(array.id, upper)))
                        } else {
                            None
                        };
                        script!(array.with_roots(&high, |array| -> Completion<()> {
                            match (&low, &high) {
                                (Some(low), Some(high)) => {
                                    script!(array.set(lower.to_string(), high.clone()));
                                    script!(array.set(upper.to_string(), low.clone()));
                                }
                                (None, Some(high)) => {
                                    script!(array.set(lower.to_string(), high.clone()));
                                    script!(array.delete(upper));
                                }
                                (Some(low), None) => {
                                    script!(array.delete(lower));
                                    script!(array.set(upper.to_string(), low.clone()));
                                }
                                (None, None) => {}
                            }
                            Ok(Ok(()))
                        }));
                        Ok(Ok(()))
                    }));
                }
            }
            _ => unreachable!(),
        }
        Ok(Ok(Value::object(self.id)))
    }

    fn copying(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let len = self.len;
        let (start, deleted, inserted) = match name {
            "with" => {
                let index = script!(self.integer(arg(args, 0)));
                let index = if index < 0.0 {
                    len as f64 + index
                } else {
                    index
                };
                if index < 0.0 || index >= len as f64 {
                    return Ok(Err(self
                        .vm
                        .make_range_error("Array.with index out of range")?));
                }
                (index as u64, 1, std::slice::from_ref(arg(args, 1)))
            }
            "toSpliced" => {
                let start = relative(script!(self.integer(arg(args, 0))), len);
                let deleted = match args.len() {
                    0 => 0,
                    1 => len - start,
                    _ => script!(self.integer(&args[1])).clamp(0.0, (len - start) as f64) as u64,
                };
                (start, deleted, args.get(2..).unwrap_or_default())
            }
            "toReversed" => (0, 0, &[][..]),
            _ => unreachable!(),
        };
        let insert_count = inserted.len() as u64;
        let new_len = len - deleted + insert_count;
        if new_len > MAX_SAFE_LENGTH {
            return Ok(Err(self
                .vm
                .type_error("array-like length exceeds the safe integer limit")?));
        }
        let result = ArrayResult {
            id: script!(self.vm.array_create(new_len)),
            fresh: true,
        };
        self.with_roots(&result, |array| {
            for index in 0..new_len {
                let value = if name == "toReversed" {
                    script!(array.get(array.id, len - 1 - index))
                } else if index < start {
                    script!(array.get(array.id, index))
                } else if index - start < insert_count {
                    inserted[(index - start) as usize].clone()
                } else {
                    script!(array.get(array.id, index - insert_count + deleted))
                };
                script!(array.define(result, index, value));
            }
            Ok(Ok(Value::object(result.id)))
        })
    }

    fn sort(&mut self, copying: bool, compare: &Value) -> Completion<Value> {
        let result = if copying {
            Some(ArrayResult {
                id: script!(self.vm.array_create(self.len)),
                fresh: true,
            })
        } else {
            None
        };
        self.with_roots(&result, |array| {
            let mut values = Vec::new();
            for index in 0..array.len {
                let value = script!(array.with_roots(&values, |array| {
                    if copying || script!(array.present(array.id, index)) {
                        return array.get(array.id, index).map(|v| v.map(Some));
                    }
                    Ok(Ok(None))
                }));
                if let Some(value) = value {
                    values.push(value);
                }
            }
            let mut buffer = Vec::with_capacity(values.len() / 2);
            let count = values.len();
            script!(array.merge_sort(compare, &mut values, &mut buffer, 0, count));
            array.with_roots(&values, |array| {
                for (index, value) in values.iter().enumerate() {
                    if let Some(result) = result {
                        script!(array.define(result, index as u64, value.clone()));
                    } else {
                        script!(array.set(index.to_string(), value.clone()));
                    }
                }
                if result.is_none() {
                    for index in count as u64..array.len {
                        script!(array.delete(index));
                    }
                }
                Ok(Ok(Value::object(
                    result.map_or(array.id, |result| result.id),
                )))
            })
        })
    }

    fn merge_sort(
        &mut self,
        compare: &Value,
        values: &mut Vec<Value>,
        buffer: &mut Vec<Value>,
        start: usize,
        end: usize,
    ) -> Completion<()> {
        if end - start < 2 {
            return Ok(Ok(()));
        }
        let mid = start + (end - start) / 2;
        script!(self.merge_sort(compare, values, buffer, start, mid));
        script!(self.merge_sort(compare, values, buffer, mid, end));
        buffer.clear();
        buffer.extend_from_slice(&values[start..mid]);
        let (mut left, mut right) = (0, mid);
        for output in start..end {
            if left == buffer.len() {
                break;
            } // Remaining right entries are in place.
            let take_left = right == end || {
                let view = (&*values, &*buffer);
                script!(self.with_roots(&view, |array| array.sort_left_first(
                    compare,
                    &buffer[left],
                    &values[right]
                )))
            };
            values[output] = if take_left {
                left += 1;
                buffer[left - 1].clone()
            } else {
                right += 1;
                values[right - 1].clone()
            };
        }
        Ok(Ok(()))
    }

    fn sort_left_first(
        &mut self,
        compare: &Value,
        left: &Value,
        right: &Value,
    ) -> Completion<bool> {
        if matches!(right.view(), ValueRef::Undefined) {
            return Ok(Ok(true));
        }
        if matches!(left.view(), ValueRef::Undefined) {
            return Ok(Ok(false));
        }
        if matches!(compare.view(), ValueRef::Undefined) {
            let left = script!(self.vm.coerce_string(self.host, self.roots, left));
            let right = script!(self.vm.coerce_string(self.host, self.roots, right));
            return Ok(Ok(left.to_utf16() <= right.to_utf16()));
        }
        let order = script!(completion(self.vm.invoke(
            self.host,
            self.roots,
            compare,
            Arguments::Values(&[left.clone(), right.clone()]),
            &Value::UNDEFINED
        )));
        let order = script!(self.vm.coerce_number(self.host, self.roots, &order));
        Ok(Ok(order <= 0.0 || order.is_nan()))
    }

    fn search(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let includes = name == "includes";
        let miss = if includes {
            Value::boolean(false)
        } else {
            Value::number(-1.0)
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
            let start = (from as u64).min(self.len - 1);
            (start, start + 1)
        } else {
            let start = (from as u64).min(self.len);
            (start, self.len - start)
        };
        let needle = arg(args, 0);
        for offset in 0..count {
            let index = if last { start - offset } else { start + offset };
            if includes || script!(self.present(self.id, index)) {
                let value = script!(self.get(self.id, index));
                let both_nan = matches!((value.view(), needle.view()), (ValueRef::Number(a), ValueRef::Number(b)) if a.is_nan() && b.is_nan());
                if strict_equal(&value, needle) || includes && both_nan {
                    return Ok(Ok(if includes {
                        Value::boolean(true)
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
            "map" => Some(script!(self.species_create(self.len))),
            "filter" | "flatMap" => Some(script!(self.species_create(0))),
            _ => None,
        };
        let mut output = 0;
        let mut accumulator = args.get(1).cloned();
        let visit_holes = matches!(name, "find" | "findIndex" | "findLast" | "findLastIndex");
        let backwards = matches!(name, "findLast" | "findLastIndex" | "reduceRight");
        let source = Value::object(self.id);
        for offset in 0..self.len {
            let index = if backwards {
                self.len - 1 - offset
            } else {
                offset
            };
            let view = (result, &accumulator);
            if !visit_holes
                && !script!(self.with_roots(&view, |array| array.present(array.id, index)))
            {
                continue;
            }
            let value = script!(self.with_roots(&view, |array| array.get(array.id, index)));
            if matches!(name, "reduce" | "reduceRight") {
                accumulator = Some(match accumulator {
                    Some(acc) => script!(completion(self.vm.invoke(
                        self.host,
                        self.roots,
                        mapper,
                        Arguments::Values(&[acc, value, number(index), source.clone()]),
                        &Value::UNDEFINED
                    ))),
                    None => value,
                });
                continue;
            }
            let preserve_value = matches!(name, "filter" | "find" | "findLast");
            let saved = preserve_value.then(|| value.clone());
            let callback_view = (result, &saved);
            let mapped = script!(completion(self.vm.invoke(
                self.host,
                &self.roots.with(&callback_view),
                mapper,
                Arguments::Values(&[value, number(index), source.clone()]),
                arg(args, 1)
            )));
            match name {
                "map" => script!(self.define(result.unwrap(), index, mapped)),
                "filter" if mapped.truthy() => {
                    script!(self.define(result.unwrap(), output, saved.unwrap()));
                    output += 1;
                }
                "flatMap" => script!(self.flatten_value(result.unwrap(), &mut output, mapped, 1.0)),
                "some" if mapped.truthy() => return Ok(Ok(Value::boolean(true))),
                "every" if !mapped.truthy() => return Ok(Ok(Value::boolean(false))),
                "find" | "findLast" if mapped.truthy() => return Ok(Ok(saved.unwrap())),
                "findIndex" | "findLastIndex" if mapped.truthy() => return Ok(Ok(number(index))),
                _ => {}
            }
        }
        Ok(match name {
            "reduce" | "reduceRight" => match accumulator {
                Some(value) => Ok(value),
                None => Err(self
                    .vm
                    .type_error("reduce of empty array without initial value")?),
            },
            "some" => Ok(Value::boolean(false)),
            "every" => Ok(Value::boolean(true)),
            "findIndex" | "findLastIndex" => Ok(Value::number(-1.0)),
            _ => Ok(result.map_or(Value::UNDEFINED, |result| Value::object(result.id))),
        })
    }

    fn combine(&mut self, name: &str, args: &[Value]) -> Completion<Value> {
        let depth = if name == "flat" {
            match arg(args, 0) {
                value if matches!(value.view(), ValueRef::Undefined) => 1.0,
                value => script!(self.integer(value)),
            }
        } else {
            0.0
        };
        let result = script!(self.species_create(0));
        let mut output = 0;
        self.with_roots(&result, |array| {
            if name == "concat" {
                let source = Value::object(array.id);
                for value in std::iter::once(&source).chain(args) {
                    if let ValueRef::Object(id) = value.view()
                        && script!(array.is_concat_spreadable(value))
                    {
                        let len =
                            script!(array.vm.length_of_array_like(array.host, array.roots, id));
                        if len > MAX_SAFE_LENGTH - output {
                            return Ok(Err(array
                                .vm
                                .type_error("concat exceeds the safe integer limit")?));
                        }
                        for index in 0..len {
                            if script!(array.present(id, index)) {
                                let item = script!(array.get(id, index));
                                script!(array.define(result, output + index, item));
                            }
                        }
                        output += len;
                    } else {
                        script!(array.flatten_value(result, &mut output, value.clone(), 0.0));
                    }
                }
                script!(array.set_on(result.id, "length".into(), number(output)));
            } else {
                for index in 0..array.len {
                    if script!(array.present(array.id, index)) {
                        let item = script!(array.get(array.id, index));
                        script!(array.flatten_value(result, &mut output, item, depth));
                    }
                }
            }
            Ok(Ok(Value::object(result.id)))
        })
    }

    fn is_concat_spreadable(&mut self, value: &Value) -> Completion<bool> {
        let spreadable = script!(completion(self.vm.read_property(
            self.host,
            self.roots,
            value,
            PropertyKeyRef::Symbol(IS_CONCAT_SPREADABLE_SYMBOL)
        )));
        Ok(Ok(if matches!(spreadable.view(), ValueRef::Undefined) {
            self.vm.is_array(value)?
        } else {
            spreadable.truthy()
        }))
    }

    fn flatten_value(
        &mut self,
        result: ArrayResult,
        output: &mut u64,
        mut value: Value,
        mut depth: f64,
    ) -> Completion<()> {
        // Explicit DFS stack: nested arrays need not fit on the native stack.
        let mut stack = Vec::<(ObjectId, std::ops::Range<u64>, f64)>::new();
        loop {
            if depth > 0.0
                && let ValueRef::Object(id) = value.view()
                && self.vm.is_array(&value)?
            {
                let view = (result, FlattenRoots(&stack));
                let len = script!(
                    self.with_roots(&view, |array| array.vm.length_of_array_like(
                        array.host,
                        array.roots,
                        id
                    ))
                );
                if depth - 1.0 == depth && stack.iter().any(|(ancestor, _, _)| *ancestor == id) {
                    return Ok(Err(self
                        .vm
                        .type_error("cannot flatten a cyclic array at this depth")?));
                }
                stack.push((id, 0..len, depth - 1.0));
            } else {
                if *output == MAX_SAFE_LENGTH {
                    return Ok(Err(self
                        .vm
                        .type_error("array output exceeds the safe integer limit")?));
                }
                script!(self.define(result, *output, value));
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
