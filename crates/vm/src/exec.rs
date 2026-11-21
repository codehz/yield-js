//! Interpreter. A step either continues, leaves the frame, or throws.
//! Handlers run in one place; call sites do not each reimplement catch.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::builtin::is_callable_object;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::string::utf16_unit_string;
use crate::bytecode::{
    BinaryOp, CallArgs, ExceptionHandler, Function, Instr, PropertyDefinition, Slot, SlotKind,
    UnaryOp,
};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, BoundObject, ClassElement, DescriptorUpdate, HAS_INSTANCE_SYMBOL, ITERATOR_SYMBOL,
    NativeObject, Object, OwnProperty, PlainObject, PrivateElement, Property, ScriptClass,
    ScriptClassSource, TO_PRIMITIVE_SYMBOL, object_id,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot as IntrinsicSlot};
use crate::ops::{self, BinaryError, unary_op};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Continuation, ExecOutcome, Invocation, Settlement};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

const FUNCTION_CALL: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "call"),
    Vm::function_call,
);
const FUNCTION_APPLY: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "apply"),
    Vm::function_apply,
);
const FUNCTION_BIND: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "bind"),
    Vm::function_bind,
);
pub(crate) const FUNCTION_HAS_INSTANCE: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::PrototypeSymbol, "hasInstance");

pub(crate) const FUNCTION_INTRINSICS: &[IntrinsicDef] = &[
    FUNCTION_CALL,
    FUNCTION_APPLY,
    FUNCTION_BIND,
    IntrinsicDef::call(FUNCTION_HAS_INSTANCE, |vm, host, roots, args, receiver| {
        vm.ordinary_has_instance(
            host,
            roots,
            args.first().unwrap_or(&Value::Undefined),
            receiver,
        )
    }),
    IntrinsicDef::call(
        IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "toString"),
        Vm::function_to_string,
    ),
];

// Virtual methods have no property slot of their own, so root their shared
// identities directly instead of allocating a function on every read.
const VIRTUAL_FUNCTION_METHODS: [(&str, IntrinsicKey); 3] = [
    ("function.call", FUNCTION_CALL.key),
    ("function.apply", FUNCTION_APPLY.key),
    ("function.bind", FUNCTION_BIND.key),
];

const fn virtual_function_root(index: usize) -> RootSpec {
    let (name, key) = VIRTUAL_FUNCTION_METHODS[index];
    RootSpec::new(name, RootKind::Native(key), Some("function.prototype"))
}

pub(crate) const FUNCTION_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "function.prototype",
        RootKind::Plain,
        Some("object.prototype"),
    ),
    virtual_function_root(0),
    virtual_function_root(1),
    virtual_function_root(2),
];

impl Vm {
    fn function_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if !self.is_callable(receiver) {
            return self.throw_type_error("incompatible Function receiver");
        }
        let mut args = args.into_iter();
        let this_arg = args.next().unwrap_or(Value::Undefined);
        self.invoke(host, roots, receiver, args.collect(), &this_arg)
    }

    fn function_apply(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if !self.is_callable(receiver) {
            return self.throw_type_error("incompatible Function receiver");
        }
        let mut args = args.into_iter();
        let this_arg = args.next().unwrap_or(Value::Undefined);
        let args = match args.next() {
            None | Some(Value::Undefined | Value::Null) => Vec::new(),
            Some(list) => {
                let live = (receiver, &this_arg, &list);
                match self.arguments_from_array_like(host, &roots.with(&live), &list)? {
                    Ok(values) => values,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
        };
        self.invoke(host, roots, receiver, args, &this_arg)
    }

    fn function_bind(
        &mut self,
        _: &mut dyn Host,
        _: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let target = match *receiver {
            Value::Object(id) if self.heap.get(id).is_ok_and(is_callable_object) => id,
            _ => return self.throw_type_error("incompatible Function receiver"),
        };
        let mut args = args.into_iter();
        let id = self.heap.alloc(Object::Bound(BoundObject {
            target,
            bound_this: args.next().unwrap_or(Value::Undefined),
            args: args.collect(),
            object: PlainObject::new(self.heap.function_prototype),
        }));
        Ok(Invocation::Value(Value::Object(id)))
    }

    fn function_to_string(
        &mut self,
        _: &mut dyn Host,
        _: &ActiveRoots<'_>,
        _: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if self.is_callable(receiver) {
            Ok(Invocation::Value(Value::String(
                "function () { [native code] }".into(),
            )))
        } else {
            self.throw_type_error("incompatible Function.prototype.toString receiver")
        }
    }
}

/// What one instruction did. `Throw` has not been caught yet.
enum Step {
    Next,
    Suspend,
    Return(Value),
    Throw(Value),
}

#[derive(Clone, Copy)]
enum Hint {
    Default,
    Number,
    String,
}

impl Hint {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Number => "number",
            Self::String => "string",
        }
    }
}

impl Vm {
    pub(crate) fn execute_frame(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        mut frame: Frame,
    ) -> Result<ExecOutcome, VmError> {
        loop {
            let instr = {
                let function = &self.build.functions[frame.function];
                if frame.pc >= function.code.len() {
                    return Ok(ExecOutcome::Return(Value::Undefined));
                }
                function.code[frame.pc].clone()
            };
            let pc = frame.pc;
            frame.pc += 1;
            match self.step(host, roots, &mut frame, instr)? {
                Step::Next => {}
                Step::Suspend => return Ok(ExecOutcome::Suspended),
                Step::Return(value) => return Ok(ExecOutcome::Return(value)),
                Step::Throw(value) => {
                    if !self.land_throw(&mut frame, pc, value.clone())? {
                        return Ok(ExecOutcome::Thrown(value));
                    }
                }
            }
        }
    }

    pub(crate) fn resume_continuation(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        continuation_id: crate::value::ContinuationId,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let continuation = self.continuations.get(&continuation_id).ok_or_else(|| {
            VmError::InvalidState(format!("continuation {continuation_id} does not exist"))
        })?;
        let function_index = self.function_index(continuation.func_id)?;
        let resume_entry = self.build.functions[function_index]
            .resumes
            .get(continuation.resume_id as usize)
            .ok_or_else(|| {
                VmError::InvalidState(format!("resume {} does not exist", continuation.resume_id))
            })?;
        if resume_entry.saved_slots.len() != continuation.values.len() {
            return Err(VmError::InvalidState(format!(
                "continuation {continuation_id} has {} values, expected {}",
                continuation.values.len(),
                resume_entry.saved_slots.len()
            )));
        }

        let async_promise = continuation.async_promise;
        let mut frame = Frame::new(
            function_index,
            &self.build.functions[function_index],
            continuation.env,
            Some(async_promise),
        );
        for (slot, value) in resume_entry
            .saved_slots
            .iter()
            .copied()
            .zip(continuation.values.iter().cloned())
        {
            frame.write(
                Slot {
                    kind: SlotKind::Persistent,
                    index: slot,
                },
                value,
            )?;
        }
        match settlement {
            Settlement::Fulfilled(value) => {
                frame.write(resume_entry.fulfill_dst, value.clone())?;
                frame.pc = resume_entry.fulfill_pc as usize;
            }
            Settlement::Rejected(value) => {
                frame.write(resume_entry.reject_dst, value.clone())?;
                frame.pc = resume_entry.reject_pc as usize;
            }
        }
        let outcome = self.execute_frame(host, roots, frame)?;
        self.settle_outcome(host, roots, async_promise, outcome)
    }

    fn step(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: Instr,
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match instr {
            Instr::Const { dst, constant } => {
                frame.write(dst, self.constant(constant)?)?;
                Ok(Step::Next)
            }
            Instr::Undefined { dst } => {
                frame.write(dst, Value::Undefined)?;
                Ok(Step::Next)
            }
            Instr::Move { dst, src } => {
                frame.write(dst, frame.read(src)?)?;
                Ok(Step::Next)
            }
            Instr::Binary { dst, op, lhs, rhs } => {
                let mut lhs = frame.read(lhs)?;
                let mut rhs = frame.read(rhs)?;
                let hint = match op {
                    BinaryOp::StrictEq | BinaryOp::StrictNe => None,
                    BinaryOp::Eq | BinaryOp::Ne => match (&lhs, &rhs) {
                        (
                            Value::Object(_),
                            Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Symbol(_),
                        )
                        | (
                            Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Symbol(_),
                            Value::Object(_),
                        ) => Some(Hint::Default),
                        _ => None,
                    },
                    BinaryOp::Add => Some(Hint::Default),
                    _ => Some(Hint::Number),
                };
                if let Some(hint) = hint {
                    // Arithmetic applies ToNumber to the left operand before the right.
                    let numeric = matches!(hint, Hint::Number)
                        && !matches!(
                            op,
                            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
                        );
                    for left in [true, false] {
                        let (value, other) = if left {
                            (&mut lhs, &rhs)
                        } else {
                            (&mut rhs, &lhs)
                        };
                        if matches!(value, Value::Object(_)) {
                            // The first coercion may produce a fresh symbol.
                            match self.primitive_from_object(
                                host,
                                &frame_roots.with(other),
                                value,
                                hint,
                            )? {
                                Invocation::Value(primitive) => *value = primitive,
                                Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                            }
                        }
                        if numeric && matches!(value, Value::Symbol(_)) {
                            return Ok(Step::Throw(self.type_error(ops::SYMBOL_TO_NUMBER)?));
                        }
                    }
                }
                let value = match ops::binary_op(op, lhs, rhs) {
                    Ok(value) => value,
                    Err(BinaryError::TypeError(message)) => {
                        return Ok(Step::Throw(self.type_error(message)?));
                    }
                    Err(BinaryError::Vm(error)) => return Err(error),
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::Unary { dst, op, src } => {
                let mut value = frame.read(src)?;
                if ops::unary_coerces(op) && matches!(value, Value::Object(_)) {
                    match self.primitive_from_object(host, frame_roots, &value, Hint::Number)? {
                        Invocation::Value(primitive) => value = primitive,
                        Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                    }
                }
                if ops::unary_coerces(op) && matches!(value, Value::Symbol(_)) {
                    return Ok(Step::Throw(self.type_error(ops::SYMBOL_TO_NUMBER)?));
                }
                frame.write(dst, self.unary(op, value))?;
                Ok(Step::Next)
            }
            Instr::ToString { dst, src } => {
                let invocation = self.string_value(host, frame_roots, frame.read_ref(src)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::ToPropertyKey { dst, src, object } => {
                if let Some(object) = object
                    && !matches!(frame.read_ref(object)?, Value::Object(_))
                {
                    return Ok(Step::Throw(
                        self.type_error("property access on a non-object")?,
                    ));
                }
                let value =
                    match self.coerce_property_key(host, frame_roots, frame.read_ref(src)?)? {
                        Ok(PropertyKey::String(key)) => Value::String(key),
                        Ok(PropertyKey::Symbol(id)) => Value::Symbol(id),
                        Err(error) => return Ok(Step::Throw(error)),
                    };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::GetImport { dst, import } => {
                let module = self.imports.get(import.module as usize).ok_or_else(|| {
                    VmError::InvalidState(format!("import module {} does not exist", import.module))
                })?;
                let declared_module = self
                    .build
                    .imports
                    .keys()
                    .nth(import.module as usize)
                    .ok_or_else(|| {
                        VmError::InvalidState(format!(
                            "import module {} is not declared",
                            import.module
                        ))
                    })?;
                if declared_module != &module.module {
                    return Err(VmError::InvalidState(format!(
                        "import module index {} is inconsistent",
                        import.module
                    )));
                }
                let value = match import.export {
                    None => Value::Object(module.namespace),
                    Some(export) => {
                        module.values.get(export as usize).cloned().ok_or_else(|| {
                            VmError::InvalidState(format!("import export {export} does not exist"))
                        })?
                    }
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::CallValue {
                dst,
                callee,
                args,
                receiver,
            } => self.call_value(host, roots, frame, dst, callee, args, receiver),
            Instr::Construct { dst, callee, args } => {
                let args = self.read_call_args(frame, &args)?;
                let callee = frame.read(callee)?;
                let invocation = self.construct_with(host, frame_roots, &callee, args, &callee)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::SetHome { function, home } => {
                let function = object_id(frame.read_ref(function)?, "method function")?;
                let home = object_id(frame.read_ref(home)?, "method home")?;
                if !matches!(self.heap.get(home)?, Object::Plain(_)) {
                    return Err(VmError::InvalidState(
                        "invalid object literal method home".into(),
                    ));
                }
                let Object::Function(function) = self.heap.get_mut(function)? else {
                    return Err(VmError::InvalidState("home requires a method".into()));
                };
                if function.home.is_some()
                    || !self
                        .build
                        .function(function.func_id)
                        .is_some_and(|f| f.is_method)
                {
                    return Err(VmError::InvalidState("home requires a new method".into()));
                }
                function.home = Some(home);
                Ok(Step::Next)
            }
            Instr::GetSuperBase { dst, home } => {
                let home = object_id(frame.read_ref(home)?, "super home")?;
                let base = self
                    .heap
                    .prototype_of(home)?
                    .map_or(Value::Null, Value::Object);
                frame.write(dst, base)?;
                Ok(Step::Next)
            }
            Instr::GetSuper {
                dst,
                base,
                key,
                receiver,
            } => {
                let Value::Object(base) = frame.read(base)? else {
                    return Ok(Step::Throw(self.type_error("super base is null")?));
                };
                let key = match self.coerce_property_key(host, frame_roots, frame.read_ref(key)?)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Step::Throw(error)),
                };
                let receiver = frame.read_ref(receiver)?;
                let result = self.read_property_with_receiver(
                    host,
                    frame_roots,
                    base,
                    receiver,
                    key.borrowed(),
                )?;
                store_invocation(frame, dst, result)
            }
            Instr::SetSuper {
                base,
                key,
                receiver,
                src,
            } => {
                let Value::Object(base) = frame.read(base)? else {
                    return Ok(Step::Throw(self.type_error("super base is null")?));
                };
                let key = match self.coerce_property_key(host, frame_roots, frame.read_ref(key)?)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Step::Throw(error)),
                };
                let result = self.set_property(
                    host,
                    frame_roots,
                    base,
                    frame.read_ref(receiver)?,
                    key,
                    frame.read(src)?,
                    true,
                )?;
                match result {
                    Invocation::Value(_) => Ok(Step::Next),
                    Invocation::Thrown(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::GetDerivedThis { dst, context } => {
                let context = object_id(frame.read_ref(context)?, "derived this context")?;
                let invocation = self.derived_this(context)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::GetSuperConstructor { dst, context } => {
                let context = object_id(frame.read_ref(context)?, "super construction context")?;
                frame.write(dst, self.super_constructor(context)?)?;
                Ok(Step::Next)
            }
            Instr::SuperConstruct {
                dst,
                context,
                callee,
                args,
            } => {
                let context = object_id(frame.read_ref(context)?, "super construction context")?;
                let args = self.read_call_args(frame, &args)?;
                let invocation = self.super_construct(
                    host,
                    frame_roots,
                    context,
                    frame.read_ref(callee)?,
                    args,
                )?;
                store_invocation(frame, dst, invocation)
            }
            Instr::Closure { dst, function } => self.make_closure(frame, dst, function),
            Instr::NewClassScope { dst, parent } => {
                let parent = self.class_scope(frame, parent)?;
                let env = self
                    .heap
                    .alloc_environment(Some(parent), Default::default());
                frame.write(dst, Value::Object(env))?;
                Ok(Step::Next)
            }
            Instr::LoadClassBinding {
                dst,
                scope,
                binding,
            } => {
                let scope = self.class_scope(frame, scope)?;
                let Some(value) = self.heap.find_binding(scope, binding)? else {
                    return Ok(Step::Throw(
                        self.reference_error("class name is not initialized")?,
                    ));
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::InitClassBinding {
                scope,
                binding,
                src,
            } => {
                let scope = self.class_scope(frame, Some(scope))?;
                let value = frame.read(src)?;
                let bindings = &mut self.heap.environment_mut(scope)?.bindings;
                let std::collections::btree_map::Entry::Vacant(entry) = bindings.entry(binding)
                else {
                    return Err(VmError::InvalidState(
                        "class binding initialized twice".into(),
                    ));
                };
                entry.insert(value);
                Ok(Step::Next)
            }
            Instr::CreateClass {
                dst,
                scope,
                constructor,
                heritage,
                default_constructor,
                private_count,
                private_binding,
            } => {
                let env = self.class_scope(frame, scope)?;
                let derived = heritage.is_some();
                let function_root = self.builtins.get("function.prototype");
                let (static_parent, instance_parent) =
                    match heritage.map(|slot| frame.read(slot)).transpose()? {
                        None => (function_root, Some(self.builtins.get("object.prototype"))),
                        Some(Value::Null) => (function_root, None),
                        Some(parent) => {
                            if !self.is_constructor(&parent)? {
                                return Ok(Step::Throw(
                                    self.type_error("class heritage is not a constructor")?,
                                ));
                            }
                            let parent_id = object_id(&parent, "class heritage")?;
                            match self.read_property(
                                host,
                                frame_roots,
                                &parent,
                                PropertyKeyRef::String("prototype"),
                            )? {
                                Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                                Invocation::Value(Value::Object(id)) => (parent_id, Some(id)),
                                Invocation::Value(Value::Null) => (parent_id, None),
                                Invocation::Value(_) => {
                                    return Ok(Step::Throw(self.type_error(
                                        "class heritage prototype must be an object or null",
                                    )?));
                                }
                            }
                        }
                    };
                let class = self.heap.alloc_function(constructor, env);
                let prototype = self.heap.alloc_plain_with(instance_parent);
                let Object::Function(function) = self.heap.get_mut(class)? else {
                    unreachable!()
                };
                function.prototype = Some(static_parent);
                function.class = Some(ScriptClass {
                    prototype,
                    source: ScriptClassSource {
                        derived,
                        default_constructor,
                        elements: Vec::new(),
                        private_count,
                        private_binding,
                        private: Default::default(),
                    },
                });
                function.home = Some(prototype);
                let name = self.function_metadata(class, "name")?;
                let length = self.function_metadata(class, "length")?;
                let metadata = Attributes {
                    configurable: true,
                    ..Attributes::FIXED
                };
                self.heap.define_data(class, "name", name, metadata)?;
                self.heap.define_data(class, "length", length, metadata)?;
                self.heap.define_data(
                    class,
                    "prototype",
                    Value::Object(prototype),
                    Attributes::FIXED,
                )?;
                self.heap.define_data(
                    prototype,
                    "constructor",
                    Value::Object(class),
                    Attributes::METHOD,
                )?;
                frame.write(dst, Value::Object(class))?;
                Ok(Step::Next)
            }
            Instr::ScopedClosure {
                dst,
                scope,
                function,
            } => {
                let env = self.class_scope(frame, Some(scope))?;
                let closure = self.materialize_function(function, env)?;
                frame.write(dst, Value::Object(closure))?;
                Ok(Step::Next)
            }
            Instr::DefineClassMethod {
                class,
                key,
                src,
                is_static,
                kind,
            } => {
                let class = object_id(&frame.read(class)?, "class method target")?;
                let key = frame.read(key)?;
                let key = converted_property_key(&key)?.to_owned();
                let src = object_id(&frame.read(src)?, "class method closure")?;
                if is_static && key.borrowed().is_str("prototype") {
                    return Ok(Step::Throw(
                        self.type_error("static class element cannot be named prototype")?,
                    ));
                }
                self.define_class_method(class, key, src, is_static, kind)?;
                Ok(Step::Next)
            }
            Instr::AddClassField {
                class,
                key,
                initializer,
                is_static,
            } => {
                let class = object_id(&frame.read(class)?, "class field target")?;
                let key = converted_property_key(frame.read_ref(key)?)?.to_owned();
                if is_static && key.borrowed().is_str("prototype") {
                    return Ok(Step::Throw(
                        self.type_error("static class element cannot be named prototype")?,
                    ));
                }
                let initializer = initializer
                    .map(|slot| object_id(frame.read_ref(slot)?, "field initializer"))
                    .transpose()?;
                self.add_class_element(
                    class,
                    ClassElement::Field {
                        key,
                        initializer,
                        is_static,
                    },
                )?;
                Ok(Step::Next)
            }
            Instr::AddClassBlock { class, body } => {
                let class = object_id(frame.read_ref(class)?, "static block target")?;
                let body = object_id(frame.read_ref(body)?, "static block body")?;
                self.add_class_element(class, ClassElement::Block { body })?;
                Ok(Step::Next)
            }
            Instr::RunClassStatic { class } => {
                let class = object_id(frame.read_ref(class)?, "static initialization target")?;
                match self.initialize_class_elements(host, frame_roots, class, class, true)? {
                    Some(error) => Ok(Step::Throw(error)),
                    None => Ok(Step::Next),
                }
            }
            Instr::AddClassPrivate {
                class,
                index,
                lexical,
                src,
                is_static,
                kind,
            } => {
                let class = object_id(frame.read_ref(class)?, "private element class")?;
                self.check_private_lexical(class, lexical, index)?;
                let closure = src
                    .map(|slot| object_id(frame.read_ref(slot)?, "private element closure"))
                    .transpose()?;
                self.add_class_private(class, index, closure, is_static, kind)?;
                Ok(Step::Next)
            }
            Instr::GetPrivate {
                dst,
                object,
                brand,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                let value =
                    self.get_private(host, frame_roots, frame.read_ref(object)?, brand, index)?;
                store_invocation(frame, dst, value)
            }
            Instr::SetPrivate {
                object,
                brand,
                src,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                match self.set_private(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    brand,
                    index,
                    frame.read(src)?,
                )? {
                    Invocation::Thrown(error) => Ok(Step::Throw(error)),
                    Invocation::Value(_) => Ok(Step::Next),
                }
            }
            Instr::HasPrivate {
                dst,
                object,
                brand,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                let Value::Object(object) = frame.read(object)? else {
                    return Ok(Step::Throw(
                        self.type_error("private brand check requires an object")?,
                    ));
                };
                let present = self
                    .heap
                    .private
                    .get(&object)
                    .is_some_and(|elements| elements.contains_key(&(brand, index)));
                frame.write(dst, Value::Bool(present))?;
                Ok(Step::Next)
            }
            Instr::NewObject { dst } => {
                let id = self.heap.alloc_plain();
                frame.write(dst, Value::Object(id))?;
                Ok(Step::Next)
            }
            Instr::NewRegExp { dst, source, flags } => {
                let id = self.alloc_regexp(&source, &flags)?;
                frame.write(dst, Value::Object(id))?;
                Ok(Step::Next)
            }
            Instr::NewArray { dst, elements } => {
                let elements = frame.read_list(&elements)?;
                let id = self.heap.alloc_array(elements);
                frame.write(dst, Value::Object(id))?;
                Ok(Step::Next)
            }
            Instr::ArrayAppend { array, src } => {
                self.append_array(frame.read_ref(array)?, frame.read_ref(src)?)?;
                Ok(Step::Next)
            }
            Instr::ArrayHole { array } => {
                let id = object_id(
                    frame.read_ref(array)?,
                    "array append target must be an object",
                )?;
                let Object::Array(array) = self.heap.get_mut(id)? else {
                    return Err(VmError::InvalidState(
                        "array append target must be an array".to_owned(),
                    ));
                };
                array.append_hole()?;
                Ok(Step::Next)
            }
            Instr::GetMember { dst, object, key } => {
                let invocation = self.read_member(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    frame.read_ref(key)?,
                )?;
                store_invocation(frame, dst, invocation)
            }
            Instr::GetDestructuredMember { dst, object, key } => {
                let key = converted_property_key(frame.read_ref(key)?)?;
                let invocation =
                    self.read_destructured_member(host, frame_roots, frame.read_ref(object)?, key)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::HasProperty { dst, object, key } => {
                let invocation = self.has_property(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    frame.read_ref(key)?,
                )?;
                store_invocation(frame, dst, invocation)
            }
            Instr::InstanceOf {
                dst,
                value,
                constructor,
            } => {
                let invocation = self.instance_of(
                    host,
                    frame_roots,
                    frame.read_ref(value)?,
                    frame.read_ref(constructor)?,
                )?;
                store_invocation(frame, dst, invocation)
            }
            Instr::DeleteMember { dst, object, key } => {
                let invocation = self.delete_property(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    frame.read_ref(key)?,
                )?;
                if matches!(invocation, Invocation::Value(Value::Bool(false))) {
                    return Ok(Step::Throw(
                        self.type_error("cannot delete a non-configurable property")?,
                    ));
                }
                store_invocation(frame, dst, invocation)
            }
            Instr::GetForInIterator { dst, source } => {
                let iterator = self.heap.alloc_for_in_iterator(frame.read_ref(source)?);
                frame.write(dst, Value::Object(iterator))?;
                Ok(Step::Next)
            }
            Instr::ForInNext {
                done,
                value,
                iterator,
            } => {
                let iterator = object_id(frame.read_ref(iterator)?, "for-in cursor")?;
                let key = match self.for_in_next(host, frame_roots, iterator)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Step::Throw(error)),
                };
                frame.write(done, Value::Bool(key.is_none()))?;
                frame.write(value, key.map_or(Value::Undefined, Value::String))?;
                Ok(Step::Next)
            }
            Instr::GetIterator {
                iterator,
                next,
                iterable,
            } => {
                let (value, next_method) =
                    match self.get_iterator(host, frame_roots, frame.read_ref(iterable)?)? {
                        Ok(record) => record,
                        Err(error) => return Ok(Step::Throw(error)),
                    };
                frame.write(iterator, value)?;
                frame.write(next, next_method)?;
                Ok(Step::Next)
            }
            Instr::IteratorNext {
                done,
                value,
                iterator,
                next,
            } => {
                let item = match self.iterator_step(
                    host,
                    frame_roots,
                    frame.read_ref(iterator)?,
                    frame.read_ref(next)?,
                )? {
                    Ok(item) => item,
                    Err(error) => return Ok(Step::Throw(error)),
                };
                frame.write(done, Value::Bool(item.is_none()))?;
                frame.write(value, item.unwrap_or(Value::Undefined))?;
                Ok(Step::Next)
            }
            Instr::IteratorSkip {
                done,
                iterator,
                next,
            } => {
                let done_value = match self.iterator_step_result(
                    host,
                    frame_roots,
                    frame.read_ref(iterator)?,
                    frame.read_ref(next)?,
                )? {
                    Ok(result) => result.is_none(),
                    Err(error) => return Ok(Step::Throw(error)),
                };
                frame.write(done, Value::Bool(done_value))?;
                Ok(Step::Next)
            }
            Instr::IteratorClose {
                iterator,
                preserve_throw,
            } => {
                let invocation = self.close_iterator(host, frame_roots, &frame.read(iterator)?)?;
                match invocation {
                    Invocation::Thrown(value) if !preserve_throw => Ok(Step::Throw(value)),
                    _ => Ok(Step::Next),
                }
            }
            Instr::GetBuiltin { dst, builtin } => {
                let value = self.builtin_value(builtin)?;
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::SetMember { object, key, src } => {
                match self.write_member(
                    host,
                    frame_roots,
                    &frame.read(object)?,
                    &frame.read(key)?,
                    frame.read(src)?,
                )? {
                    Invocation::Value(_) => Ok(Step::Next),
                    Invocation::Thrown(value) => Ok(Step::Throw(value)),
                }
            }
            Instr::DefineProperty {
                object,
                key,
                src,
                kind,
            } => {
                let key = match self.coerce_property_key(host, frame_roots, frame.read_ref(key)?)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Step::Throw(error)),
                };
                self.define_property(frame.read_ref(object)?, key, frame.read(src)?, kind)?;
                Ok(Step::Next)
            }
            Instr::CopyDataProperties { target, source } => {
                let target = object_id(frame.read_ref(target)?, "spread target must be an object")?;
                match self.copy_data_properties(
                    host,
                    frame_roots,
                    target,
                    frame.read_ref(source)?,
                    &BTreeSet::new(),
                )? {
                    Invocation::Value(_) => Ok(Step::Next),
                    Invocation::Thrown(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::AssertObjectCoercible { src } => {
                if matches!(frame.read_ref(src)?, Value::Null | Value::Undefined) {
                    return Ok(Step::Throw(
                        self.type_error("Cannot destructure null or undefined")?,
                    ));
                }
                Ok(Step::Next)
            }
            Instr::ObjectRest { dst, src, excluded } => {
                let source = frame.read_ref(src)?;
                if matches!(source, Value::Null | Value::Undefined) {
                    return Ok(Step::Throw(
                        self.type_error("Cannot destructure null or undefined")?,
                    ));
                }
                let excluded = excluded
                    .into_iter()
                    .map(|slot| converted_property_key(frame.read_ref(slot)?))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let target = self.heap.alloc_plain();
                match self.copy_data_properties(host, frame_roots, target, source, &excluded)? {
                    Invocation::Value(_) => frame.write(dst, Value::Object(target))?,
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            Instr::GetBinding { dst, binding } => {
                let env = frame
                    .env
                    .ok_or_else(|| VmError::InvalidState("binding read without env".to_owned()))?;
                frame.write(dst, self.heap.get_binding(env, binding)?)?;
                Ok(Step::Next)
            }
            Instr::SetBinding { binding, src } => {
                let env = frame
                    .env
                    .ok_or_else(|| VmError::InvalidState("binding write without env".to_owned()))?;
                self.heap.set_binding(env, binding, frame.read(src)?)?;
                Ok(Step::Next)
            }
            Instr::DeclareBinding { binding } => {
                let env = frame.env.ok_or_else(|| {
                    VmError::InvalidState("binding declaration without env".to_owned())
                })?;
                self.heap.declare_binding(env, binding)?;
                Ok(Step::Next)
            }
            Instr::Await { input, resume, .. } => self.suspend(host, roots, frame, input, resume),
            Instr::Jump { target } => {
                frame.pc = target as usize;
                Ok(Step::Next)
            }
            Instr::BranchFalse { cond, target } => {
                if !frame.read(cond)?.truthy() {
                    frame.pc = target as usize;
                }
                Ok(Step::Next)
            }
            Instr::Return { src } => Ok(Step::Return(frame.read(src)?)),
            Instr::Throw { src } => Ok(Step::Throw(frame.read(src)?)),
            Instr::ThrowDeleteSuper => Ok(Step::Throw(
                self.reference_error("cannot delete a super property")?,
            )),
            Instr::ThrowAssignmentToConst => Ok(Step::Throw(
                self.type_error("Assignment to constant variable.")?,
            )),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    fn call_value(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        dst: Slot,
        callee: Slot,
        args: CallArgs,
        receiver: Option<Slot>,
    ) -> Result<Step, VmError> {
        let args = self.read_call_args(frame, &args)?;
        let callee = frame.read(callee)?;
        let receiver = match receiver {
            Some(slot) => frame.read(slot)?,
            None => Value::Undefined,
        };
        let invocation = self.invoke(host, &roots.with(frame), &callee, args, &receiver)?;
        store_invocation(frame, dst, invocation)
    }

    fn read_call_args(&self, frame: &Frame, args: &CallArgs) -> Result<Vec<Value>, VmError> {
        match args {
            CallArgs::Slots(slots) => frame.read_list(slots),
            CallArgs::Spread(slot) => {
                let message = "spread arguments require a dense data array";
                let id = object_id(frame.read_ref(*slot)?, message)?;
                let Object::Array(array) = self.heap.get(id)? else {
                    return Err(VmError::InvalidState(message.to_owned()));
                };
                array.dense_values()
            }
        }
    }

    pub(crate) fn get_iterator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterable: &Value,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        let roots = &roots.with(iterable);
        let method = if matches!(iterable, Value::Object(_) | Value::String(_)) {
            match self.read_property(
                host,
                roots,
                iterable,
                PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
            )? {
                Invocation::Value(method) => method,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            return Ok(Err(self.type_error("value is not iterable")?));
        };
        self.get_iterator_from_method(host, roots, iterable, &method)
    }

    pub(crate) fn get_iterator_from_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterable: &Value,
        method: &Value,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        let result = self.invoke(host, roots, method, Vec::new(), iterable)?;
        let iterator =
            self.require_iterator_object(result, "Symbol.iterator must return an object")?;
        let iterator = match iterator {
            Invocation::Value(iterator) => iterator,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&iterator);
        match self.read_property(host, roots, &iterator, PropertyKeyRef::String("next"))? {
            Invocation::Value(next) => Ok(Ok((iterator, next))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    /// A failed step is not a consumer-side abrupt exit and does not close the iterator.
    pub(crate) fn iterator_step(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterator: &Value,
        next: &Value,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let result = match self.iterator_step_result(host, roots, iterator, next)? {
            Ok(Some(result)) => result,
            other => return Ok(other),
        };
        let roots = &roots.with(&result);
        match self.read_property(host, roots, &result, PropertyKeyRef::String("value"))? {
            Invocation::Value(value) => Ok(Ok(Some(value))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn iterator_step_result(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterator: &Value,
        next: &Value,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let result = self.invoke(host, roots, next, Vec::new(), iterator)?;
        let result =
            match self.require_iterator_object(result, "iterator next must return an object")? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        let roots = &roots.with(&result);
        match self.read_property(host, roots, &result, PropertyKeyRef::String("done"))? {
            Invocation::Value(done) if done.truthy() => Ok(Ok(None)),
            Invocation::Value(_) => Ok(Ok(Some(result))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn append_array(&mut self, array: &Value, value: &Value) -> Result<(), VmError> {
        let id = object_id(array, "array append target must be an object")?;
        let Object::Array(array) = self.heap.get_mut(id)? else {
            return Err(VmError::InvalidState(
                "array append target must be an array".to_owned(),
            ));
        };
        array.append(value.clone())
    }

    pub(crate) fn close_iterator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterator: &Value,
    ) -> Result<Invocation, VmError> {
        // Valid bytecode produced by the compiler only supplies a GetIterator result.
        if !matches!(iterator, Value::Object(_)) {
            return self.throw_type_error("iterator must be an object");
        }
        let roots = &roots.with(iterator);
        let method =
            match self.read_property(host, roots, iterator, PropertyKeyRef::String("return"))? {
                Invocation::Value(Value::Undefined | Value::Null) => {
                    return Ok(Invocation::Value(Value::Undefined));
                }
                Invocation::Value(method) => method,
                thrown => return Ok(thrown),
            };
        let invocation = self.invoke(host, roots, &method, Vec::new(), iterator)?;
        self.require_iterator_object(invocation, "iterator return must return an object")
    }

    pub(crate) fn string_value(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Invocation, VmError> {
        Ok(match self.coerce_string(host, roots, value)? {
            Ok(string) => Invocation::Value(Value::String(string)),
            Err(error) => Invocation::Thrown(error),
        })
    }

    pub(crate) fn coerce_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::String)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(value.as_ref(), Value::Symbol(_)) {
            return Ok(Err(self.type_error(ops::SYMBOL_TO_STRING)?));
        }
        Ok(Ok(ops::to_js_string(&value)))
    }

    pub(crate) fn coerce_number(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::Number)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(value.as_ref(), Value::Symbol(_)) {
            return Ok(Err(self.type_error(ops::SYMBOL_TO_NUMBER)?));
        }
        Ok(Ok(ops::to_number(&value)?))
    }

    pub(crate) fn coerce_property_key(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<PropertyKey, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::String)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(match value.as_ref() {
            Value::Symbol(id) => PropertyKey::Symbol(*id),
            _ => PropertyKey::String(ops::to_js_string(&value)),
        }))
    }

    fn coerce_primitive<'a>(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &'a Value,
        hint: Hint,
    ) -> Result<Result<Cow<'a, Value>, Value>, VmError> {
        if !matches!(value, Value::Object(_)) {
            return Ok(Ok(Cow::Borrowed(value)));
        }
        Ok(
            match self.primitive_from_object(host, roots, value, hint)? {
                Invocation::Value(value) => Ok(Cow::Owned(value)),
                Invocation::Thrown(error) => Err(error),
            },
        )
    }

    fn primitive_from_object(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        hint: Hint,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(value);
        match self.read_property(
            host,
            roots,
            value,
            PropertyKeyRef::Symbol(TO_PRIMITIVE_SYMBOL),
        )? {
            Invocation::Value(Value::Undefined | Value::Null) => {}
            Invocation::Value(method) => {
                let result = self.invoke(
                    host,
                    roots,
                    &method,
                    vec![Value::String(hint.as_str().into())],
                    value,
                )?;
                return match result {
                    Invocation::Value(Value::Object(_)) => {
                        self.throw_type_error("Cannot convert object to primitive value")
                    }
                    result => Ok(result),
                };
            }
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        }
        let methods = match hint {
            Hint::String => ["toString", "valueOf"],
            _ => ["valueOf", "toString"],
        };
        for name in methods {
            let method =
                match self.read_property(host, roots, value, PropertyKeyRef::String(name))? {
                    Invocation::Value(method) => method,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
            if !self.is_callable(&method) {
                continue;
            }
            match self.invoke(host, roots, &method, Vec::new(), value)? {
                Invocation::Value(Value::Object(_)) => {}
                Invocation::Value(primitive) => return Ok(Invocation::Value(primitive)),
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        self.throw_type_error("Cannot convert object to primitive value")
    }

    fn require_iterator_object(
        &mut self,
        invocation: Invocation,
        message: &str,
    ) -> Result<Invocation, VmError> {
        match invocation {
            Invocation::Value(Value::Object(_)) | Invocation::Thrown(_) => Ok(invocation),
            Invocation::Value(_) => self.throw_type_error(message),
        }
    }

    pub(crate) fn read_member(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(object, Value::Null | Value::Undefined) {
            return self.throw_type_error("property access on a non-object");
        }
        match key {
            Value::String(key) => {
                return self.read_property(host, roots, object, PropertyKeyRef::JsString(key));
            }
            Value::Symbol(id) => {
                return self.read_property(host, roots, object, PropertyKeyRef::Symbol(*id));
            }
            _ => {}
        }
        let roots = &roots.with(object);
        let key = match self.coerce_property_key(host, roots, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.read_property(host, roots, object, key.borrowed())
    }

    fn read_destructured_member(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        if matches!(object, Value::Null | Value::Undefined) {
            return self.throw_type_error("Cannot destructure null or undefined");
        }
        self.read_property(host, roots, object, key)
    }

    pub(crate) fn read_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        // Primitive strings resolve indexed reads without allocating wrappers.
        if let Value::String(text) = object {
            if key.is_str("length") {
                return Ok(Invocation::Value(Value::Number(text.unit_len() as f64)));
            }
            if let Some(unit) = ops::canonical_index_key(key).and_then(|index| text.unit_at(index))
            {
                return Ok(Invocation::Value(Value::String(utf16_unit_string(unit))));
            }
        }
        if let Some(prototype) = self.primitive_prototype(object) {
            return self.read_property_with_receiver(host, roots, prototype, object, key);
        }
        let Value::Object(id) = *object else {
            return self.throw_type_error("property access on a non-object");
        };
        self.read_property_with_receiver(host, roots, id, object, key)
    }

    pub(crate) fn read_property_with_receiver(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        receiver: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        let mut current = Some(target);
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Native(NativeObject::Host {
                target: crate::binding::NativeTarget::Export(export),
                ..
            }) = *object
                && key.is_str("prototype")
                && let Some(class) = self.bindings.export_class(export)
            {
                return Ok(Invocation::Value(Value::Object(
                    self.host_classes[class].prototype,
                )));
            }
            if let Object::Proxy {
                target, handler, ..
            } = *object
            {
                return self.proxy_get(host, roots, target, handler, key, receiver);
            }
            if self.has_own_function_metadata(id, key)? {
                let name = if key.is_str("name") { "name" } else { "length" };
                return Ok(Invocation::Value(self.function_metadata(id, name)?));
            }
            if let Some(property) = object.own_property(key) {
                return self.read_property_value(host, roots, receiver, property.into_owned());
            }
            current = object.prototype();
        }
        if let Some(method) = self.virtual_function_method(target, key) {
            return Ok(Invocation::Value(Value::Object(method)));
        }
        Ok(Invocation::Value(Value::Undefined))
    }

    pub(crate) fn read_array_index(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        index: usize,
    ) -> Result<Invocation, VmError> {
        let Object::Array(array) = self.heap.get(id)? else {
            unreachable!("array index reader requires an array")
        };
        let receiver = Value::Object(id);
        if let Some(property) = array.own_index(index).map(OwnProperty::into_owned) {
            self.read_property_value(host, roots, &receiver, property)
        } else {
            self.read_property(
                host,
                roots,
                &receiver,
                PropertyKeyRef::String(&index.to_string()),
            )
        }
    }

    fn read_property_value(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        property: Property,
    ) -> Result<Invocation, VmError> {
        match property {
            Property::Data(value) => Ok(Invocation::Value(value)),
            Property::Accessor { get: Some(get), .. } => {
                self.invoke(host, roots, &Value::Object(get), Vec::new(), receiver)
            }
            Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::Undefined)),
        }
    }

    fn instance_of(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        constructor: &Value,
    ) -> Result<Invocation, VmError> {
        self.has_instance(host, roots, value, constructor, false)
    }

    fn ordinary_has_instance(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        constructor: &Value,
    ) -> Result<Invocation, VmError> {
        self.has_instance(host, roots, value, constructor, true)
    }

    fn has_instance(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        constructor: &Value,
        mut ordinary: bool,
    ) -> Result<Invocation, VmError> {
        let not_callable = "right-hand side of 'instanceof' is not callable";
        let Value::Object(mut target) = *constructor else {
            return if ordinary {
                Ok(Invocation::Value(Value::Bool(false)))
            } else {
                self.throw_type_error("right-hand side of 'instanceof' is not an object")
            };
        };
        let live = (value, constructor);
        let roots = &roots.with(&live);
        let mut remaining = self.heap.object_count();
        loop {
            let receiver = Value::Object(target);
            if !ordinary {
                let hook = match self.read_property(
                    host,
                    &roots.with(&receiver),
                    &receiver,
                    PropertyKeyRef::Symbol(HAS_INSTANCE_SYMBOL),
                )? {
                    Invocation::Value(hook) => hook,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
                if !matches!(hook, Value::Undefined | Value::Null) {
                    if !self.is_callable(&hook) {
                        return self.throw_type_error("Symbol.hasInstance is not callable");
                    }
                    // Inline the default intrinsic after lookup so a chain of
                    // bound functions does not recurse through native calls.
                    let Value::Object(hook_id) = hook else {
                        unreachable!("callable hook")
                    };
                    ordinary = matches!(
                        self.heap.get(hook_id)?,
                        Object::Native(NativeObject::Intrinsic { intrinsic, .. })
                            if *intrinsic == FUNCTION_HAS_INSTANCE.link()
                    );
                    if !ordinary {
                        return Ok(
                            match self.invoke(host, roots, &hook, vec![value.clone()], &receiver)? {
                                Invocation::Value(result) => {
                                    Invocation::Value(Value::Bool(result.truthy()))
                                }
                                thrown @ Invocation::Thrown(_) => thrown,
                            },
                        );
                    }
                }
            }
            let object = self.heap.get(target)?;
            if !is_callable_object(object) {
                return if ordinary {
                    Ok(Invocation::Value(Value::Bool(false)))
                } else {
                    self.throw_type_error(not_callable)
                };
            }
            let Object::Bound(bound) = object else {
                break;
            };
            remaining -= 1;
            if remaining == 0 {
                return Err(VmError::InvalidState(
                    "cyclic bound function target chain".into(),
                ));
            }
            target = bound.target;
            // Bound targets re-enter the full protocol, including their hooks.
            ordinary = false;
        }

        let Value::Object(mut current) = *value else {
            return Ok(Invocation::Value(Value::Bool(false)));
        };
        let prototype = match self.read_property(
            host,
            roots,
            &Value::Object(target),
            PropertyKeyRef::String("prototype"),
        )? {
            Invocation::Value(Value::Object(id)) => id,
            Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
            Invocation::Value(_) => {
                return self
                    .throw_type_error("'instanceof' constructor prototype is not an object");
            }
        };
        // Prototype links are kept acyclic by set_prototype and snapshot restore.
        while let Some(parent) = self.heap.prototype_of(current)? {
            if parent == prototype {
                return Ok(Invocation::Value(Value::Bool(true)));
            }
            current = parent;
        }
        Ok(Invocation::Value(Value::Bool(false)))
    }

    fn has_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = *object else {
            return self.throw_type_error("right-hand side of 'in' is not an object");
        };
        let roots = &roots.with(object);
        let key = match self.coerce_property_key(host, roots, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.has_property_key(host, roots, id, key.borrowed())
    }

    pub(crate) fn has_property_key(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        let mut current = Some(target);
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Proxy {
                target, handler, ..
            } = *object
            {
                return self.proxy_presence(host, roots, target, handler, key, false);
            }
            if self.has_own_function_metadata(id, key)? || object.own_property(key).is_some() {
                return Ok(Invocation::Value(Value::Bool(true)));
            }
            current = object.prototype();
        }
        Ok(Invocation::Value(Value::Bool(
            self.virtual_function_method(target, key).is_some(),
        )))
    }

    pub(crate) fn delete_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(target, Value::Null | Value::Undefined) {
            return self.throw_type_error("cannot delete a property of null or undefined");
        }
        let roots = &roots.with(target);
        let coerced;
        let key = match key {
            Value::String(key) => PropertyKeyRef::JsString(key),
            Value::Symbol(id) => PropertyKeyRef::Symbol(*id),
            _ => {
                coerced = match self.coerce_property_key(host, roots, key)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                coerced.borrowed()
            }
        };
        if let Value::Object(id) = *target
            && let Object::Proxy {
                target, handler, ..
            } = *self.heap.get(id)?
        {
            return self.proxy_presence(host, roots, target, handler, key, true);
        }
        let deleted = match (target, key) {
            (Value::Object(id), key) if self.has_own_function_metadata(*id, key)? => false,
            (Value::Object(id), key) => self.heap.delete_own(*id, key)?,
            // ToObject on a string would expose fixed UTF-16 indices and length.
            (Value::String(string), key) => {
                !key.is_str("length")
                    && !ops::canonical_index_key(key).is_some_and(|index| index < string.unit_len())
            }
            _ => true,
        };
        Ok(Invocation::Value(Value::Bool(deleted)))
    }

    pub(crate) fn has_own_function_metadata(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<bool, VmError> {
        Ok((key.is_str("name") || key.is_str("length"))
            && (id == self.builtins.get("function.prototype")
                || (is_callable_object(self.heap.get(id)?)
                    && !self.heap.function(id).is_some_and(|function| {
                        function.class.is_some() || function.has_class_metadata
                    }))))
    }

    pub(crate) fn function_metadata(&self, id: ObjectId, key: &str) -> Result<Value, VmError> {
        let (object, bound_args, prefix) = match self.heap.get(id)? {
            Object::Bound(bound) => (self.heap.get(bound.target)?, bound.args.len(), "bound "),
            object => (object, 0, ""),
        };
        if key == "name"
            && !prefix.is_empty()
            && let Some(OwnProperty::Stored(Property::Data(Value::String(name)), _)) =
                object.own_property(PropertyKeyRef::String("name"))
        {
            let mut result = JsString::from(prefix);
            result.push_str(name);
            return Ok(Value::String(result));
        }
        let (name, length) = match object {
            Object::Function(function) => {
                let function = self.build.function(function.func_id).ok_or_else(|| {
                    VmError::InvalidState("function metadata without bytecode".into())
                })?;
                // The compiler appends the function ID to nested bytecode names.
                let name = match function.name.rsplit_once('@') {
                    Some((name, id)) if id.parse::<u32>() == Ok(function.func_id) => name,
                    _ => &function.name,
                };
                (name, function.length)
            }
            Object::Native(NativeObject::Intrinsic { intrinsic, .. }) => {
                let key = intrinsic.definition().key;
                let length = match (key.owner, key.slot, key.member) {
                    ("RegExp", _, "") => 2,
                    ("RegExp", _, "exec" | "test" | "escape") => 1,
                    ("Object", IntrinsicSlot::Static, "values" | "entries" | "fromEntries") => 1,
                    (
                        "Object",
                        IntrinsicSlot::Prototype,
                        "isPrototypeOf" | "propertyIsEnumerable",
                    ) => 1,
                    _ => 0,
                };
                let name = if key.member.is_empty() {
                    key.owner
                } else {
                    key.member
                };
                (name, length)
            }
            _ => ("", 0), // The shared prototype and Promise resolving functions.
        };
        Ok(if key == "name" {
            Value::String([prefix, name].concat().into())
        } else {
            let bound_args = u16::try_from(bound_args).unwrap_or(u16::MAX);
            Value::Number(f64::from(length.saturating_sub(bound_args)))
        })
    }

    fn virtual_function_method(&self, id: ObjectId, key: PropertyKeyRef<'_>) -> Option<ObjectId> {
        if !self.heap.get(id).is_ok_and(is_callable_object) {
            return None;
        }
        VIRTUAL_FUNCTION_METHODS
            .iter()
            .find(|(_, intrinsic)| key.is_str(intrinsic.member))
            .map(|(root, _)| self.builtins.get(root))
    }

    fn write_member(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        if matches!(object, Value::Null | Value::Undefined) {
            return self.throw_type_error("property write on a non-object");
        }
        let roots = &roots.with(object);
        let key = match self.coerce_property_key(host, &roots.with(&value), key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let id = match *object {
            Value::Object(id) => id,
            _ => {
                let key = key.borrowed();
                if let Value::String(text) = object
                    && (key.is_str("length")
                        || ops::canonical_index_key(key)
                            .is_some_and(|index| index < text.unit_len()))
                {
                    return self.throw_type_error("Cannot assign to a read only string property");
                }
                self.primitive_prototype(object)
                    .expect("non-nullish primitive")
            }
        };
        self.set_property(host, roots, id, object, key, value, true)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    pub(crate) fn set_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        receiver: &Value,
        key: PropertyKey,
        value: Value,
        strict: bool,
    ) -> Result<Invocation, VmError> {
        let success = Invocation::Value(Value::Bool(true));
        let reject = |vm: &mut Self, message: &str| {
            if strict {
                vm.throw_type_error(message)
            } else {
                Ok(Invocation::Value(Value::Bool(false)))
            }
        };

        // Lookup starts at the target. A setter sees the receiver as `this`;
        // a data write goes to the receiver, not to the descriptor's owner.
        let mut current = Some(target);
        let mut property = None;
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Proxy {
                target, handler, ..
            } = *object
            {
                return self.proxy_set(
                    host,
                    roots,
                    (target, handler),
                    receiver,
                    key,
                    value,
                    strict,
                );
            }
            if self.has_own_function_metadata(id, key.borrowed())? {
                return reject(self, "Cannot assign to read only function metadata");
            }
            property = object.own_property(key.borrowed());
            if property.is_some() {
                break;
            }
            current = object.prototype();
        }
        match property.as_ref().and_then(OwnProperty::setter) {
            Some(Some(set)) => {
                return match self.invoke(host, roots, &Value::Object(set), vec![value], receiver)? {
                    Invocation::Value(_) => Ok(success),
                    thrown @ Invocation::Thrown(_) => Ok(thrown),
                };
            }
            Some(None) => return reject(self, "Cannot set property without a setter"),
            None if property.is_some_and(|property| !property.attributes().writable) => {
                if strict && let PropertyKey::String(key) = &key {
                    return self
                        .throw_type_error(&format!("Cannot assign to read only property '{key}'"));
                }
                return reject(self, "Cannot assign to read only symbol property");
            }
            None => {}
        }

        let Value::Object(id) = *receiver else {
            return reject(
                self,
                "Cannot write a data property on a non-object receiver",
            );
        };
        if id != target && self.has_own_function_metadata(id, key.borrowed())? {
            return reject(self, "Cannot overwrite receiver function metadata");
        }
        if matches!(self.heap.get(id)?, Object::Proxy { .. }) {
            let live = (&key, &value, receiver);
            let exists = match self.own_descriptor(host, &roots.with(&live), id, key.borrowed())? {
                Err(error) => return Ok(Invocation::Thrown(error)),
                Ok(Some((property, attrs)))
                    if !attrs.writable || !matches!(property, Property::Data(_)) =>
                {
                    return reject(self, "Cannot overwrite receiver property");
                }
                Ok(descriptor) => descriptor.is_some(),
            };
            let update = DescriptorUpdate {
                value: Some(value),
                writable: (!exists).then_some(true),
                enumerable: (!exists).then_some(true),
                configurable: (!exists).then_some(true),
                ..DescriptorUpdate::default()
            };
            return match self.try_apply_property_update(host, roots, id, key, update)? {
                Ok(true) => Ok(success),
                Ok(false) => reject(self, "Cannot define receiver property"),
                Err(error) => Ok(Invocation::Thrown(error)),
            };
        }
        let object = self.heap.get(id)?;
        // With the same extensible target and receiver, the lookup above
        // already rules out an incompatible own property.
        if id != target || !object.is_extensible() {
            match object.own_property(key.borrowed()) {
                Some(property)
                    if property.setter().is_some() || !property.attributes().writable =>
                {
                    return reject(self, "Cannot overwrite receiver property");
                }
                None if matches!(object, Object::Native(_)) => {
                    return reject(self, "native handles do not expose script properties");
                }
                None if !object.is_extensible() => {
                    return reject(self, "Cannot add property to a non-extensible object");
                }
                _ => {}
            }
        }
        if let Object::Array(array) = object {
            if key.borrowed().is_str("length") {
                let update = DescriptorUpdate {
                    value: Some(value),
                    ..DescriptorUpdate::default()
                };
                return match self.try_apply_property_update(host, roots, id, key, update)? {
                    Ok(true) => Ok(success),
                    Ok(false) => reject(self, "Cannot define property"),
                    Err(error) => Ok(Invocation::Thrown(error)),
                };
            }
            if !array.length_writable
                && ops::canonical_index_key(key.borrowed())
                    .is_some_and(|index| index < u32::MAX as usize && index >= array.length)
            {
                return reject(self, "Cannot extend array with read only length");
            }
        }
        self.heap.set_own(id, key, value)?;
        Ok(success)
    }

    fn copy_data_properties(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        source: &Value,
        excluded: &BTreeSet<PropertyKeyRef<'_>>,
    ) -> Result<Invocation, VmError> {
        let live = (target, source);
        let roots = &roots.with(&live);
        match source {
            Value::Object(id) => {
                // Capture the order once; an earlier getter may delete a later key
                // or change its enumerability before it is visited.
                let keys = match self.own_keys(host, roots, *id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let mut keys = keys.into_iter();
                while let Some(key) = keys.next() {
                    if excluded.contains(&key.borrowed()) {
                        continue;
                    }
                    // A getter can delete keys from the source. Keep the
                    // current key and the unvisited snapshot keys alive.
                    let live = (keys.as_slice(), &key);
                    let key_roots = &roots.with(&live);
                    match self.own_descriptor(host, key_roots, *id, key.borrowed())? {
                        Ok(Some((_, attrs))) if attrs.enumerable => {}
                        Ok(_) => continue,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                    match self.read_property(host, key_roots, source, key.borrowed())? {
                        Invocation::Value(value) => {
                            self.heap
                                .define_property(target, key, Property::Data(value))?;
                        }
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    }
                }
            }
            Value::String(text) => {
                for (index, unit) in text.units().enumerate() {
                    let key = index.to_string();
                    if excluded.contains(&PropertyKeyRef::String(&key)) {
                        continue;
                    }
                    self.heap.define_property(
                        target,
                        PropertyKey::String(key.into()),
                        Property::Data(Value::String(utf16_unit_string(unit))),
                    )?;
                }
            }
            // Nullish values and the remaining primitive wrappers have no keys to copy.
            _ => {}
        }
        Ok(Invocation::Value(Value::Undefined))
    }

    fn define_property(
        &mut self,
        object: &Value,
        key: PropertyKey,
        value: Value,
        kind: PropertyDefinition,
    ) -> Result<(), VmError> {
        let id = object_id(object, "property definition on a non-object")?;
        let property = match kind {
            PropertyDefinition::Data => Property::Data(value),
            PropertyDefinition::Get | PropertyDefinition::Set => {
                let target = object_id(&value, "accessor definition requires a script method")?;
                if !self
                    .heap
                    .get(target)?
                    .as_function()
                    .and_then(|function| self.build.function(function.func_id))
                    .is_some_and(Function::is_accessor_method)
                {
                    return Err(VmError::InvalidState(
                        "accessor definition requires a synchronous script method".to_owned(),
                    ));
                }
                Property::Accessor {
                    get: (kind == PropertyDefinition::Get).then_some(target),
                    set: (kind == PropertyDefinition::Set).then_some(target),
                }
            }
        };
        self.heap.define_property(id, key, property)
    }

    fn class_scope(&self, frame: &Frame, scope: Option<Slot>) -> Result<ObjectId, VmError> {
        let env = match scope {
            Some(scope) => object_id(&frame.read(scope)?, "class scope")?,
            None => frame.env.ok_or_else(|| {
                VmError::InvalidState("class scope without an environment".into())
            })?,
        };
        self.heap.environment(env)?;
        Ok(env)
    }

    fn add_class_element(&mut self, id: ObjectId, element: ClassElement) -> Result<(), VmError> {
        let home = {
            let Object::Function(function) = self.heap.get_mut(id)? else {
                return Err(VmError::InvalidState(
                    "class element requires a function".into(),
                ));
            };
            let class = function
                .class
                .as_mut()
                .ok_or_else(|| VmError::InvalidState("class element requires a class".into()))?;
            match element {
                ClassElement::Field {
                    is_static: false, ..
                } => class.prototype,
                ClassElement::PrivateField { index, .. }
                    if !class.source.private[&index].is_static =>
                {
                    class.prototype
                }
                _ => id,
            }
        };
        if let Some(closure) = element.closure() {
            let Object::Function(function) = self.heap.get_mut(closure)? else {
                return Err(VmError::InvalidState(
                    "class element requires a closure".into(),
                ));
            };
            if !self
                .build
                .function(function.func_id)
                .is_some_and(Function::is_accessor_method)
            {
                return Err(VmError::InvalidState(
                    "class element closure must be synchronous".into(),
                ));
            }
            function.home = Some(home);
        }
        let Object::Function(function) = self.heap.get_mut(id)? else {
            unreachable!()
        };
        function
            .class
            .as_mut()
            .expect("validated class")
            .source
            .elements
            .push(element);
        Ok(())
    }

    /// Initialize each own element in source order, stopping at the first throw.
    pub(crate) fn initialize_class_elements(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        class: ObjectId,
        receiver: ObjectId,
        is_static: bool,
    ) -> Result<Option<Value>, VmError> {
        let live = (class, receiver);
        let roots = &roots.with(&live);
        let record = self
            .heap
            .function(class)
            .and_then(|f| f.class.as_ref())
            .ok_or_else(|| VmError::InvalidState("initialization requires a class".into()))?;
        let element_count = record.source.elements.len();
        if !self
            .heap
            .install_private_methods(class, receiver, is_static)
        {
            return Ok(Some(
                self.type_error("private member is already installed")?,
            ));
        }
        for index in 0..element_count {
            let element = &self
                .heap
                .function(class)
                .and_then(|f| f.class.as_ref())
                .expect("class record")
                .source
                .elements[index];
            let (key, private, closure) = match element {
                ClassElement::Field {
                    key,
                    initializer,
                    is_static: field_static,
                } if *field_static == is_static => (Some(key.clone()), None, *initializer),
                ClassElement::PrivateField { index, initializer }
                    if self.private_class(class, *index)?.source.private[index].is_static
                        == is_static =>
                {
                    (None, Some(*index), *initializer)
                }
                ClassElement::Block { body } if is_static => (None, None, Some(*body)),
                _ => continue,
            };
            let value = if let Some(closure) = closure {
                match self.invoke(
                    host,
                    &roots.with(&key),
                    &Value::Object(closure),
                    Vec::new(),
                    &Value::Object(receiver),
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Some(error)),
                }
            } else {
                Value::Undefined
            };
            if let Some(index) = private {
                if !self.stamp_private(receiver, class, index, PrivateElement::Field(value)) {
                    return Ok(Some(self.type_error("private field is already installed")?));
                }
                continue;
            }
            if let Some(key) = key {
                let update = DescriptorUpdate {
                    value: Some(value),
                    writable: Some(true),
                    enumerable: Some(true),
                    configurable: Some(true),
                    ..Default::default()
                };
                if let Invocation::Thrown(error) =
                    self.apply_property_update(host, roots, receiver, key, update)?
                {
                    return Ok(Some(error));
                }
            }
        }
        Ok(None)
    }

    fn define_class_method(
        &mut self,
        class: ObjectId,
        key: PropertyKey,
        src: ObjectId,
        is_static: bool,
        kind: PropertyDefinition,
    ) -> Result<(), VmError> {
        let prototype = self
            .heap
            .function(class)
            .and_then(|f| f.class.as_ref())
            .ok_or_else(|| VmError::InvalidState("class member requires a class".into()))?
            .prototype;
        let target = if is_static { class } else { prototype };
        let method = self
            .heap
            .function(src)
            .and_then(|f| self.build.function(f.func_id));
        if !method.is_some_and(|f| {
            f.is_method && (kind == PropertyDefinition::Data || f.is_accessor_method())
        }) {
            return Err(VmError::InvalidState(
                "class member requires a script method".into(),
            ));
        }
        let mut name = JsString::from(match kind {
            PropertyDefinition::Data => "",
            PropertyDefinition::Get => "get ",
            PropertyDefinition::Set => "set ",
        });
        match &key {
            PropertyKey::String(key) => name.push_str(key),
            PropertyKey::Symbol(symbol) => {
                if let Some(Some(description)) = self.heap.symbols.get(symbol) {
                    name.push_str(&"[".into());
                    name.push_str(description);
                    name.push_str(&"]".into());
                }
            }
        }
        let length = self.function_metadata(src, "length")?;
        let Object::Function(function) = self.heap.get_mut(src)? else {
            unreachable!()
        };
        function.home = Some(target);
        function.has_class_metadata = true;
        let metadata = Attributes {
            configurable: true,
            ..Attributes::FIXED
        };
        self.heap
            .define_data(src, "name", Value::String(name), metadata)?;
        self.heap.define_data(src, "length", length, metadata)?;
        let mut update = DescriptorUpdate {
            enumerable: Some(false),
            configurable: Some(true),
            ..Default::default()
        };
        match kind {
            PropertyDefinition::Data => {
                update.value = Some(Value::Object(src));
                update.writable = Some(true);
            }
            PropertyDefinition::Get => update.get = Some(Some(src)),
            PropertyDefinition::Set => update.set = Some(Some(src)),
        }
        if !self.heap.apply_descriptor(target, key, update)? {
            return Err(VmError::InvalidState(
                "invalid class method definition".into(),
            ));
        }
        Ok(())
    }

    fn make_closure(
        &mut self,
        frame: &mut Frame,
        dst: Slot,
        func_id: u32,
    ) -> Result<Step, VmError> {
        let env = frame.env.ok_or_else(|| {
            VmError::InvalidState("closure without an invocation environment".into())
        })?;
        let id = self.materialize_function(func_id, env)?;
        frame.write(dst, Value::Object(id))?;
        Ok(Step::Next)
    }

    fn suspend(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &Frame,
        input: Slot,
        resume: u32,
    ) -> Result<Step, VmError> {
        let input_value = frame.read(input)?;
        let async_promise = frame
            .async_promise
            .ok_or_else(|| VmError::InvalidState("Await outside an async frame".to_owned()))?;
        let func_id = self.build.functions[frame.function].func_id;
        let resume_entry = self.build.functions[frame.function]
            .resumes
            .get(resume as usize)
            .cloned()
            .ok_or_else(|| VmError::InvalidState(format!("resume {resume} does not exist")))?;
        let values = frame.read_saved(&resume_entry.saved_slots)?;
        let prepared = match self.prepare_await(host, &roots.with(frame), input_value)? {
            Ok(prepared) => prepared,
            Err(error) => return Ok(Step::Throw(error)),
        };
        let (waiting_on, plain) = match prepared {
            AwaitPrepared::Promise(id) => (Some(id), Value::Undefined),
            AwaitPrepared::Plain(value) => (None, value),
        };
        let continuation_id = self.alloc_continuation();
        self.continuations.insert(
            continuation_id,
            Continuation {
                async_promise,
                func_id,
                resume_id: resume,
                waiting_on,
                env: frame.env,
                values,
            },
        );
        self.schedule_resume(continuation_id, waiting_on, plain)?;
        Ok(Step::Suspend)
    }

    fn schedule_resume(
        &mut self,
        continuation: crate::value::ContinuationId,
        waiting_on: Option<ObjectId>,
        plain: Value,
    ) -> Result<(), VmError> {
        let Some(promise) = waiting_on else {
            self.enqueue(continuation, Settlement::Fulfilled(plain));
            return Ok(());
        };
        self.react(promise, crate::heap::ReactionJob::Await(continuation))
    }

    fn unary(&self, operator: UnaryOp, value: Value) -> Value {
        if matches!(operator, UnaryOp::TypeOf) && self.is_callable(&value) {
            return Value::String("function".into());
        }
        unary_op(operator, value)
    }

    fn constant(&self, index: u32) -> Result<Value, VmError> {
        let constant = self
            .build
            .constants
            .get(index as usize)
            .ok_or_else(|| VmError::InvalidState(format!("constant {index} does not exist")))?;
        Ok(match constant {
            crate::bytecode::Constant::Null => Value::Null,
            crate::bytecode::Constant::Bool(value) => Value::Bool(*value),
            crate::bytecode::Constant::Number(value) => Value::Number(*value),
            crate::bytecode::Constant::String(value) => Value::String(value.clone()),
        })
    }

    fn land_throw(&mut self, frame: &mut Frame, pc: usize, value: Value) -> Result<bool, VmError> {
        let Some((dst, target)) =
            handler_landing(&self.build.functions[frame.function].handlers, pc)
        else {
            return Ok(false);
        };
        frame.write(dst, value)?;
        frame.pc = target as usize;
        Ok(true)
    }
}

fn converted_property_key(value: &Value) -> Result<PropertyKeyRef<'_>, VmError> {
    match value {
        Value::String(key) => Ok(PropertyKeyRef::JsString(key)),
        Value::Symbol(id) => Ok(PropertyKeyRef::Symbol(*id)),
        _ => Err(VmError::InvalidState(
            "destructuring requires an already converted property key".to_owned(),
        )),
    }
}

fn store_invocation(frame: &mut Frame, dst: Slot, invocation: Invocation) -> Result<Step, VmError> {
    match invocation {
        Invocation::Value(value) => {
            frame.write(dst, value)?;
            Ok(Step::Next)
        }
        Invocation::Thrown(value) => Ok(Step::Throw(value)),
    }
}

fn handler_landing(handlers: &[ExceptionHandler], pc: usize) -> Option<(Slot, u32)> {
    let mut selected: Option<&ExceptionHandler> = None;
    for handler in handlers {
        if (handler.start_pc as usize) <= pc && pc < handler.end_pc as usize {
            let replace = selected.is_none_or(|current| {
                handler.start_pc > current.start_pc
                    || (handler.start_pc == current.start_pc && handler.end_pc < current.end_pc)
            });
            if replace {
                selected = Some(handler);
            }
        }
    }
    selected.map(|handler| (handler.dst, handler.target_pc))
}
