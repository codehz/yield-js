//! HTML structured clone: convert transfer arguments, clone the graph, then
//! commit transfers in list order. Only the middle pass walks object edges.

use std::collections::BTreeMap;

use super::FUNCTION_PARENT;
use super::binary::{ARRAY_BUFFER_PROTOTYPE, Completion, DATA_VIEW_PROTOTYPE, FAMILIES};
use super::globals::Owner;
use super::roots::{RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{
    ArrayBufferObject, Attributes, BoxedPrimitive, DataViewObject, Object, PlainObject,
    TypedArrayObject, ViewRange, view_range,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const KEY: IntrinsicKey = IntrinsicKey::new("structuredClone", Slot::Global, "");
pub(super) const OWNER: Owner = Owner::new(
    "structuredClone",
    &[RootSpec::new(
        "structuredClone",
        RootKind::Native(KEY),
        FUNCTION_PARENT,
    )],
    &[&[IntrinsicDef::call(KEY, 1, |vm, host, roots, args, _| {
        Ok(match clone(vm, host, &roots.with(&args), &args)? {
            Ok(value) => Invocation::Value(value),
            Err(error) => Invocation::Thrown(error),
        })
    })]],
    None,
);

macro_rules! normal {
    ($result:expr) => {
        match $result? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

fn get(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    key: PropertyKeyRef<'_>,
) -> Completion<Value> {
    Ok(match vm.read_property(host, roots, receiver, key)? {
        Invocation::Value(value) => Ok(value),
        Invocation::Thrown(error) => Err(error),
    })
}

fn transfer_list(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<Vec<ObjectId>> {
    match options.view() {
        ValueRef::Undefined | ValueRef::Null => return Ok(Ok(Vec::new())),
        ValueRef::Object(_) => {}
        _ => {
            return Ok(Err(
                vm.type_error(roots, "structuredClone options must be an object")?
            ));
        }
    }
    let iterable = normal!(get(
        vm,
        host,
        roots,
        options,
        PropertyKeyRef::String("transfer")
    ));
    match iterable.view() {
        ValueRef::Undefined => return Ok(Ok(Vec::new())),
        ValueRef::Object(_) => {}
        _ => {
            return Ok(Err(
                vm.type_error(roots, "transfer must be an iterable object")?
            ));
        }
    }
    let (iterator, next) = normal!(vm.get_iterator(host, roots, &iterable));
    let iterator_roots = (&iterator, &next);
    let roots = &roots.with(&iterator_roots);
    let mut ids = Vec::new();
    loop {
        let roots = &roots.with(&ids);
        let Some(value) = normal!(vm.iterator_step(host, roots, &iterator, &next)) else {
            return Ok(Ok(ids));
        };
        let ValueRef::Object(id) = value.view() else {
            let error = vm.type_error(roots, "transfer entries must be objects")?;
            // IteratorClose preserves the original throw even if return throws.
            vm.close_iterator(host, &roots.with(&error), &iterator)?;
            return Ok(Err(error));
        };
        ids.push(id);
    }
}

pub(super) fn clone_message(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Completion<Value> {
    match Graph::default().walk(vm, host, &roots.with(value), value.clone())? {
        Ok(cloned) => Ok(Ok(cloned)),
        // A getter can throw an arbitrary source-side object. Transport errors
        // must not carry that object across the link either.
        Err(_) => rejected(vm),
    }
}

fn rejected<T>(vm: &mut Vm) -> Completion<T> {
    Ok(Err(vm.alloc_dom_exception(
        "DataCloneError".into(),
        "Value could not be cloned".into(),
    )))
}

fn clone(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Completion<Value> {
    let Some(value) = args.first() else {
        return Ok(Err(
            vm.type_error(roots, "structuredClone requires a value")?
        ));
    };
    let transfers = normal!(transfer_list(
        vm,
        host,
        roots,
        args.get(1).unwrap_or(&Value::UNDEFINED)
    ));
    let mut graph = Graph::default();
    for &source in &transfers {
        if graph.memory.contains_key(&source) {
            return rejected(vm);
        }
        let target = match vm.heap.get(source)? {
            Object::ArrayBuffer(_) => alloc_buffer(vm, None, None),
            Object::ReadableStream(_) | Object::WritableStream(_) | Object::TransformStream(_) => {
                vm.allocate_transfer_proxy(source)?
            }
            _ => return rejected(vm),
        };
        graph.memory.insert(source, target);
    }
    let result = normal!(graph.walk(vm, host, &roots.with(&transfers), value.clone()));
    for source in transfers {
        let target = graph.memory[&source];
        if !matches!(vm.heap.get(source)?, Object::ArrayBuffer(_)) {
            if !super::streams::transfer::transferable(vm, source)? {
                return rejected(vm);
            }
            let live = (&graph, &result);
            vm.commit_stream_transfer(&roots.with(&live), source, target)?;
            continue;
        }
        let Object::ArrayBuffer(buffer) = vm.heap.get_mut(source)? else {
            unreachable!()
        };
        let Some(bytes) = buffer.bytes.take() else {
            return rejected(vm);
        };
        let max = buffer.max_byte_length;
        let Object::ArrayBuffer(buffer) = vm.heap.get_mut(target)? else {
            unreachable!()
        };
        buffer.bytes = Some(bytes);
        buffer.max_byte_length = max;
    }
    Ok(Ok(result))
}

#[derive(Default)]
struct Graph {
    memory: BTreeMap<ObjectId, ObjectId>,
    work: Vec<Work>,
}

enum Work {
    Visit(Value, Destination),
    ReadProperty {
        source: ObjectId,
        target: ObjectId,
        key: PropertyKey,
    },
}

enum Destination {
    Root,
    Property(ObjectId, PropertyKey, Attributes),
    MapKey(ObjectId),
    MapValue(ObjectId),
    SetElement(ObjectId),
}

impl TraceRoots for Graph {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        for (&source, &target) in &self.memory {
            (source, target).trace(tracer)?;
        }
        for work in &self.work {
            if let Work::Visit(value, _) = work {
                value.trace(tracer)?;
            }
        }
        Ok(())
    }
}

impl Graph {
    fn walk(
        &mut self,
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: Value,
    ) -> Completion<Value> {
        let mut result = Value::UNDEFINED;
        self.work.push(Work::Visit(value, Destination::Root));
        while let Some(work) = self.work.pop() {
            match work {
                Work::Visit(value, destination) => {
                    let value = match value.view() {
                        ValueRef::Symbol(_) => return rejected(vm),
                        ValueRef::Object(source) => {
                            Value::object(normal!(self.object(vm, host, roots, source)))
                        }
                        _ => value,
                    };
                    match destination {
                        Destination::Root => result = value,
                        Destination::Property(target, key, attributes) => {
                            vm.heap.define_data(target, key, value, attributes)?
                        }
                        Destination::MapKey(target) => {
                            let Object::Map { entries, .. } = vm.heap.get_mut(target)? else {
                                unreachable!()
                            };
                            entries.push(Some((value, Value::UNDEFINED)));
                        }
                        Destination::MapValue(target) => {
                            let Object::Map { entries, .. } = vm.heap.get_mut(target)? else {
                                unreachable!()
                            };
                            entries
                                .last_mut()
                                .and_then(Option::as_mut)
                                .expect("cloned Map key")
                                .1 = value;
                        }
                        Destination::SetElement(target) => {
                            let Object::Set { elements, .. } = vm.heap.get_mut(target)? else {
                                unreachable!()
                            };
                            elements.push(Some(value));
                        }
                    }
                }
                Work::ReadProperty {
                    source,
                    target,
                    key,
                } => {
                    if vm.heap.get(source)?.own_property(key.borrowed()).is_some() {
                        let value = normal!(get(
                            vm,
                            host,
                            &roots.with(self),
                            &Value::object(source),
                            key.borrowed()
                        ));
                        self.work.push(Work::Visit(
                            value,
                            Destination::Property(target, key, Attributes::ORDINARY),
                        ));
                    }
                }
            }
        }
        Ok(Ok(result))
    }

    fn object(
        &mut self,
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
    ) -> Completion<ObjectId> {
        if let Some(&target) = self.memory.get(&source) {
            return Ok(Ok(target));
        }
        if source == vm.builtins.get("globalThis")
            || vm.heap.generator(source).is_some()
            || vm.host_instances.contains_key(&source)
        {
            return rejected(vm);
        }
        let target = match vm.heap.get(source)? {
            Object::ReadableStream(_) | Object::WritableStream(_) | Object::TransformStream(_) => {
                return rejected(vm);
            }
            Object::Event { .. }
            | Object::EventTarget { .. }
            | Object::Performance { .. }
            | Object::AbortController { .. }
            | Object::AbortSignal { .. } => return rejected(vm),
            Object::Plain(object) if object.is_raw_json || object.module_namespace.is_some() => {
                return rejected(vm);
            }
            Object::DomException { name, message, .. } => {
                let ValueRef::Object(id) =
                    vm.alloc_dom_exception(name.clone(), message.clone()).view()
                else {
                    unreachable!()
                };
                id
            }
            object if object.has_error_brand() => normal!(self.error(vm, host, roots, source)),
            Object::Plain(_) => {
                let target = vm
                    .heap
                    .alloc_plain_with(Some(vm.builtins.get("object.prototype")));
                self.properties(vm, source, target)?;
                target
            }
            Object::Array(array) => {
                let length = array.length;
                let target = vm.heap.alloc_array(Vec::new());
                let Object::Array(array) = vm.heap.get_mut(target)? else {
                    unreachable!()
                };
                array.length = length;
                self.properties(vm, source, target)?;
                target
            }
            Object::Map { .. } => {
                let target = vm.heap.alloc(Object::Map {
                    object: PlainObject::new(Some(vm.builtins.get("map.prototype"))),
                    entries: Vec::new(),
                });
                let Object::Map { entries, .. } = vm.heap.get(source)? else {
                    unreachable!()
                };
                // Push a snapshot in reverse order; each key's entire subtree runs before its value.
                for (key, value) in entries.iter().rev().flatten() {
                    self.work
                        .push(Work::Visit(value.clone(), Destination::MapValue(target)));
                    self.work
                        .push(Work::Visit(key.clone(), Destination::MapKey(target)));
                }
                target
            }
            Object::Set { .. } => {
                let target = vm.heap.alloc(Object::Set {
                    object: PlainObject::new(Some(vm.builtins.get("set.prototype"))),
                    elements: Vec::new(),
                });
                let Object::Set { elements, .. } = vm.heap.get(source)? else {
                    unreachable!()
                };
                self.work.extend(
                    elements
                        .iter()
                        .rev()
                        .flatten()
                        .map(|value| Work::Visit(value.clone(), Destination::SetElement(target))),
                );
                target
            }
            Object::Boxed { primitive, .. } => {
                let prototype = match primitive {
                    BoxedPrimitive::String(_) => "string.prototype",
                    BoxedPrimitive::Number(_) => "number.prototype",
                    BoxedPrimitive::Bool(_) => "boolean.prototype",
                    BoxedPrimitive::BigInt(_) => "bigint.prototype",
                    BoxedPrimitive::Symbol(_) => return rejected(vm),
                };
                vm.heap.alloc(Object::Boxed {
                    primitive: primitive.clone(),
                    object: PlainObject::new(Some(vm.builtins.get(prototype))),
                })
            }
            Object::Date { time, .. } => vm.heap.alloc(Object::Date {
                time: *time,
                object: PlainObject::new(Some(vm.builtins.get("date.prototype"))),
            }),
            Object::RegExp {
                source,
                flags,
                compiled,
                ..
            } => vm.alloc_compiled_regexp(
                source.clone(),
                *flags,
                compiled.clone(),
                vm.builtins.get("regexp.prototype"),
            )?,
            Object::ArrayBuffer(_) => return self.buffer(vm, source),
            Object::TypedArray(view) => {
                let (buffer, byte_offset, byte_length, kind) =
                    (view.buffer, view.byte_offset, view.byte_length, view.kind);
                let buffer =
                    normal!(self.view_buffer(vm, buffer, byte_offset, byte_length, kind.bytes()));
                let prototype = FAMILIES
                    .iter()
                    .find(|family| family.kind == kind)
                    .expect("typed array family")
                    .prototype;
                vm.heap.alloc(Object::TypedArray(TypedArrayObject {
                    object: PlainObject::new(Some(vm.builtins.get(prototype))),
                    buffer,
                    byte_offset,
                    byte_length,
                    kind,
                }))
            }
            Object::DataView(view) => {
                let (buffer, byte_offset, byte_length) =
                    (view.buffer, view.byte_offset, view.byte_length);
                let buffer = normal!(self.view_buffer(vm, buffer, byte_offset, byte_length, 1));
                vm.heap.alloc(Object::DataView(DataViewObject {
                    object: PlainObject::new(Some(vm.builtins.get(DATA_VIEW_PROTOTYPE))),
                    buffer,
                    byte_offset,
                    byte_length,
                }))
            }
            _ => return rejected(vm),
        };
        self.memory.insert(source, target);
        Ok(Ok(target))
    }

    fn properties(&mut self, vm: &Vm, source: ObjectId, target: ObjectId) -> Result<(), VmError> {
        let start = self.work.len();
        self.work.extend(
            vm.heap
                .get(source)?
                .own_keys(true)
                .map(|key| Work::ReadProperty {
                    source,
                    target,
                    key,
                }),
        );
        self.work[start..].reverse();
        Ok(())
    }

    fn buffer(&mut self, vm: &mut Vm, source: ObjectId) -> Completion<ObjectId> {
        if let Some(&target) = self.memory.get(&source) {
            return Ok(Ok(target));
        }
        let Object::ArrayBuffer(buffer) = vm.heap.get(source)? else {
            unreachable!()
        };
        let Some(bytes) = &buffer.bytes else {
            return rejected(vm);
        };
        let target = alloc_buffer(vm, Some(bytes.clone()), buffer.max_byte_length);
        self.memory.insert(source, target);
        Ok(Ok(target))
    }

    fn view_buffer(
        &mut self,
        vm: &mut Vm,
        source: ObjectId,
        offset: usize,
        length: Option<usize>,
        width: usize,
    ) -> Completion<ObjectId> {
        let Object::ArrayBuffer(buffer) = vm.heap.get(source)? else {
            unreachable!()
        };
        if !matches!(
            view_range(buffer.bytes.as_deref(), offset, length, width),
            ViewRange::Bytes { .. }
        ) {
            return rejected(vm);
        }
        self.buffer(vm, source)
    }

    fn error(
        &mut self,
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
    ) -> Completion<ObjectId> {
        let receiver = Value::object(source);
        let name = {
            let root_values = (&*self, &receiver);
            let roots = &roots.with(&root_values);
            normal!(get(
                vm,
                host,
                roots,
                &receiver,
                PropertyKeyRef::String("name")
            ))
        };
        let prototype = match name.view() {
            ValueRef::String(name) if name == "EvalError" => "evalerror.prototype",
            ValueRef::String(name) if name == "RangeError" => "rangeerror.prototype",
            ValueRef::String(name) if name == "ReferenceError" => "referenceerror.prototype",
            ValueRef::String(name) if name == "SyntaxError" => "syntaxerror.prototype",
            ValueRef::String(name) if name == "TypeError" => "typeerror.prototype",
            ValueRef::String(name) if name == "URIError" => "urierror.prototype",
            _ => "error.prototype",
        };
        let source_object = vm.heap.get(source)?;
        let message = match own_data(source_object, "message").cloned() {
            Some(message) => {
                let root_values = (&*self, &receiver, &message);
                let roots = &roots.with(&root_values);
                Some(normal!(vm.coerce_string(host, roots, &message)))
            }
            None => None,
        };
        let source_object = vm.heap.get(source)?;
        let stack = own_data(source_object, "stack")
            .filter(|value| matches!(value.view(), ValueRef::String(_)))
            .cloned();
        let cause = own_data(source_object, "cause").cloned();
        let mut object = PlainObject::new(Some(vm.builtins.get(prototype)));
        object.is_error = true;
        let target = vm.heap.alloc(Object::Plain(object));
        if let Some(message) = message {
            vm.heap.define_data(
                target,
                "message",
                Value::string(message),
                Attributes::METHOD,
            )?;
        }
        if let Some(stack) = stack {
            vm.heap
                .define_data(target, "stack", stack, Attributes::METHOD)?;
        }
        if let Some(cause) = cause {
            self.work.push(Work::Visit(
                cause,
                Destination::Property(target, "cause".into(), Attributes::METHOD),
            ));
        }
        Ok(Ok(target))
    }
}

fn own_data<'a>(object: &'a Object, key: &str) -> Option<&'a Value> {
    match object.own_property(PropertyKeyRef::String(key))? {
        crate::heap::OwnProperty::Stored(crate::heap::Property::Data(value), _) => Some(value),
        _ => None,
    }
}

fn alloc_buffer(vm: &mut Vm, bytes: Option<Vec<u8>>, max_byte_length: Option<usize>) -> ObjectId {
    vm.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
        object: PlainObject::new(Some(vm.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
        bytes,
        max_byte_length,
    }))
}
