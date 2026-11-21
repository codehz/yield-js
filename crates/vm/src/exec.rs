//! Interpreter. A step either continues, leaves the frame, or throws.
//! Handlers run in one place; call sites do not each reimplement catch.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use crate::builtin::is_callable_object;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::string::utf16_unit_string;
use crate::bytecode::{
    AccessorPrefix, BinaryOp, CallArgs, ExceptionHandler, Function, Instr, PropertyDefinition,
    Slot, SlotKind, UnaryOp,
};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{
    AsyncCompletion, AsyncRequest, Attributes, BoundObject, ClassElement, DescriptorUpdate,
    GeneratorStatus, HAS_INSTANCE_SYMBOL, ITERATOR_SYMBOL, NativeObject, Object, OwnProperty,
    PlainObject, PrivateElement, Property, ScriptClass, ScriptClassSource, TO_PRIMITIVE_SYMBOL,
    TypedIndex, object_id, string_own_property,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot as IntrinsicSlot};
use crate::ops::{self, BinaryError, unary_op};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Continuation, ContinuationTarget, ExecOutcome, Invocation, Settlement};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const FUNCTION_CALL: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "call"),
    1,
    Vm::function_call,
);
const FUNCTION_APPLY: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "apply"),
    2,
    Vm::function_apply,
);
const FUNCTION_BIND: IntrinsicDef = IntrinsicDef::call(
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "bind"),
    1,
    Vm::function_bind,
);
pub(crate) const FUNCTION_HAS_INSTANCE: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::PrototypeSymbol, "hasInstance");

pub(crate) const FUNCTION_CTOR: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::Global, "");
const FUNCTION_PROTO: IntrinsicKey = IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "");
pub(crate) const THROW_TYPE_ERROR: IntrinsicKey =
    IntrinsicKey::new("ThrowTypeError", IntrinsicSlot::Prototype, "");
pub(crate) const FUNCTION_METHODS: [IntrinsicKey; 4] = [
    FUNCTION_CALL.key,
    FUNCTION_APPLY.key,
    FUNCTION_BIND.key,
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "toString"),
];
pub(crate) const FUNCTION_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        FUNCTION_CTOR,
        1,
        |vm, _, _, _, _| vm.throw_type_error("Dynamic function construction is disabled"),
        |vm, _, _, _, _| vm.throw_type_error("Dynamic function construction is disabled"),
    )
    .without_new_target_prototype(),
    IntrinsicDef::call(FUNCTION_PROTO, 0, |_, _, _, _, _| {
        Ok(Invocation::Value(Value::UNDEFINED))
    }),
    IntrinsicDef::call(THROW_TYPE_ERROR, 0, |vm, _, _, _, _| {
        vm.throw_type_error("Restricted function property")
    }),
    FUNCTION_CALL,
    FUNCTION_APPLY,
    FUNCTION_BIND,
    IntrinsicDef::call(
        FUNCTION_HAS_INSTANCE,
        1,
        |vm, host, roots, args, receiver| {
            vm.ordinary_has_instance(
                host,
                roots,
                args.first().unwrap_or(&Value::UNDEFINED),
                receiver,
            )
        },
    ),
    IntrinsicDef::call(FUNCTION_METHODS[3], 0, Vm::function_to_string),
];

pub(crate) const FUNCTION_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        "function.prototype",
        RootKind::Native(FUNCTION_PROTO),
        Some("object.prototype"),
    ),
    RootSpec::new(
        "function",
        RootKind::Native(FUNCTION_CTOR),
        Some("function.prototype"),
    )
    .fixed("function.prototype"),
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
        let this_arg = args.next().unwrap_or(Value::UNDEFINED);
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
        let this_arg = args.next().unwrap_or(Value::UNDEFINED);
        let args = match args.next() {
            None => Vec::new(),
            Some(list) if matches!(list.view(), ValueRef::Undefined | ValueRef::Null) => Vec::new(),
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
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let target = match receiver.view() {
            ValueRef::Object(id) if self.heap.get(id).is_ok_and(is_callable_object) => id,
            _ => return self.throw_type_error("incompatible Function receiver"),
        };
        let object = self.heap.get(target)?;
        let prototype = object.prototype();
        let own_length = object
            .own_property(PropertyKeyRef::String("length"))
            .is_some();
        let live = (&args, prototype);
        let roots = &roots.with(&live);
        let length = if own_length {
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("length"))? {
                Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
                Invocation::Value(value) => match value.view() {
                    ValueRef::Number(n) => {
                        (n.trunc() - args.len().saturating_sub(1) as f64).max(0.0)
                    }
                    _ => 0.0,
                },
            }
        } else {
            0.0
        };
        let mut name = JsString::from("bound ");
        match self.read_property(host, roots, receiver, PropertyKeyRef::String("name"))? {
            Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
            Invocation::Value(value) => {
                if let ValueRef::String(target_name) = value.view() {
                    name.push_str(target_name);
                }
            }
        }
        let mut args = args.into_iter();
        let id = self.heap.alloc(Object::Bound(BoundObject {
            target,
            bound_this: args.next().unwrap_or(Value::UNDEFINED),
            args: args.collect(),
            object: PlainObject::callable(prototype, length, name.clone()),
            initial_name: name,
        }));
        Ok(Invocation::Value(Value::object(id)))
    }

    fn function_to_string(
        &mut self,
        _: &mut dyn Host,
        _: &ActiveRoots<'_>,
        _: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error("incompatible Function.prototype.toString receiver");
        };
        if !is_callable_object(self.heap.get(id)?) {
            return self.throw_type_error("incompatible Function.prototype.toString receiver");
        }
        let name = self.initial_function_name(id)?;
        let mut text = JsString::from("function ");
        text.push_str(&name);
        text.push_str(&"() { [native code] }".into());
        Ok(Invocation::Value(Value::string(text)))
    }

    fn initial_function_name(&self, id: ObjectId) -> Result<JsString, VmError> {
        Ok(match self.heap.get(id)? {
            Object::Function(function) => match &function.dynamic_name {
                Some(name) => name.clone(),
                None => self.build.functions[self.function_index(function.func_id)?]
                    .initial_name
                    .as_str()
                    .into(),
            },
            Object::Bound(bound) => bound.initial_name.clone(),
            Object::Native(NativeObject::Intrinsic { intrinsic, .. }) => {
                intrinsic.definition().key.initial_name()
            }
            Object::Native(NativeObject::Host { target, .. }) => {
                crate::vm::host_function_metadata(&self.bindings, *target)
                    .ok_or_else(|| {
                        VmError::InvalidState("missing host function declaration".into())
                    })?
                    .0
            }
            _ => "".into(),
        })
    }

    pub(crate) fn install_script_metadata(
        &mut self,
        id: ObjectId,
        func_id: u32,
    ) -> Result<(), VmError> {
        let length = self.build.functions[self.function_index(func_id)?].length;
        let name = self.initial_function_name(id)?;
        self.heap.define_data(
            id,
            "length",
            Value::number(f64::from(length)),
            Attributes::FUNCTION_METADATA,
        )?;
        self.heap.define_data(
            id,
            "name",
            Value::string(name),
            Attributes::FUNCTION_METADATA,
        )
    }
}

/// What one instruction did. `Throw` has not been caught yet.
enum Step {
    Next,
    Suspend,
    Yield { value: Value, delegate: bool },
    Return(Value),
    Throw(Value),
}

#[derive(Clone, Copy)]
pub(crate) enum Hint {
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
                    return Ok(ExecOutcome::Return(Value::UNDEFINED));
                }
                function.code[frame.pc].clone()
            };
            let pc = frame.pc;
            frame.pc += 1;
            match self.step(host, roots, &mut frame, instr)? {
                Step::Next => {}
                Step::Suspend => return Ok(ExecOutcome::Suspended),
                Step::Yield { value, delegate } => {
                    return Ok(ExecOutcome::Yielded { value, delegate });
                }
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

        let target = continuation.target;
        let mut frame = Frame::new(
            function_index,
            &self.build.functions[function_index],
            continuation.env,
            Some(target),
        );
        if let ContinuationTarget::Generator(id) = target {
            // The resumed body still suspends into the same generator object.
            frame.generator = Some(id);
        }
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
        match target {
            ContinuationTarget::Promise(promise) => {
                self.settle_outcome(host, roots, promise, outcome)
            }
            ContinuationTarget::Generator(id) => {
                self.finish_async_generator_body(id, outcome)?;
                self.drive_async_generator(host, roots, id)
            }
        }
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
                frame.write(dst, Value::UNDEFINED)?;
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
                    BinaryOp::Eq | BinaryOp::Ne => match (lhs.view(), rhs.view()) {
                        (
                            ValueRef::Object(_),
                            ValueRef::String(_)
                            | ValueRef::Number(_)
                            | ValueRef::BigInt(_)
                            | ValueRef::Bool(_)
                            | ValueRef::Symbol(_),
                        )
                        | (
                            ValueRef::String(_)
                            | ValueRef::Number(_)
                            | ValueRef::BigInt(_)
                            | ValueRef::Bool(_)
                            | ValueRef::Symbol(_),
                            ValueRef::Object(_),
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
                        if matches!(value.view(), ValueRef::Object(_)) {
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
                        if numeric && matches!(value.view(), ValueRef::Symbol(_)) {
                            return Ok(Step::Throw(self.type_error(ops::SYMBOL_TO_NUMBER)?));
                        }
                    }
                }
                let value = match ops::binary_op(op, lhs, rhs) {
                    Ok(value) => value,
                    Err(BinaryError::TypeError(message)) => {
                        return Ok(Step::Throw(self.type_error(message)?));
                    }
                    Err(BinaryError::RangeError(message)) => {
                        return Ok(Step::Throw(self.make_range_error(message)?));
                    }
                    Err(BinaryError::Vm(error)) => return Err(error),
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::Unary { dst, op, src } => {
                let mut value = frame.read(src)?;
                if ops::unary_coerces(op) && matches!(value.view(), ValueRef::Object(_)) {
                    match self.primitive_from_object(host, frame_roots, &value, Hint::Number)? {
                        Invocation::Value(primitive) => value = primitive,
                        Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                    }
                }
                if ops::unary_coerces(op) && matches!(value.view(), ValueRef::Symbol(_)) {
                    return Ok(Step::Throw(self.type_error(ops::SYMBOL_TO_NUMBER)?));
                }
                let value = match self.unary(op, value) {
                    Ok(value) => value,
                    Err(BinaryError::TypeError(message)) => {
                        return Ok(Step::Throw(self.type_error(message)?));
                    }
                    Err(BinaryError::RangeError(message)) => {
                        return Ok(Step::Throw(self.make_range_error(message)?));
                    }
                    Err(BinaryError::Vm(error)) => return Err(error),
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::ToString { dst, src } => {
                let invocation = self.string_value(host, frame_roots, frame.read_ref(src)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::ToPropertyKey { dst, src, object } => {
                if let Some(object) = object
                    && !matches!(frame.read_ref(object)?.view(), ValueRef::Object(_))
                {
                    return Ok(Step::Throw(
                        self.type_error("property access on a non-object")?,
                    ));
                }
                let value =
                    match self.coerce_property_key(host, frame_roots, frame.read_ref(src)?)? {
                        Ok(PropertyKey::String(key)) => Value::string(key),
                        Ok(PropertyKey::Symbol(id)) => Value::symbol(id),
                        Err(error) => return Ok(Step::Throw(error)),
                    };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::GetImport { dst, import } => {
                let value = self.read_host_import(import)?;
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::GetModuleBinding {
                dst,
                module,
                binding,
            } => {
                let env = self.module_environment(module)?;
                let Some(value) = self.heap.get_module_binding(env, binding)? else {
                    return Ok(Step::Throw(self.reference_error(&format!(
                        "module binding {binding} is not initialized"
                    ))?));
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::GetModuleNamespace { dst, module } => {
                frame.write(dst, self.module_namespace(module)?)?;
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
            Instr::SetFunctionName {
                function,
                key,
                prefix,
            } => {
                let id = object_id(frame.read_ref(function)?, "named function")?;
                let name = self
                    .property_function_name(converted_property_key(frame.read_ref(key)?)?, prefix);
                let Object::Function(function) = self.heap.get_mut(id)? else {
                    return Err(VmError::InvalidState(
                        "name requires a script function".into(),
                    ));
                };
                function.dynamic_name = Some(name.clone());
                self.heap.define_data(
                    id,
                    "name",
                    Value::string(name),
                    Attributes::FUNCTION_METADATA,
                )?;
                Ok(Step::Next)
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
                    .map_or(Value::NULL, Value::object);
                frame.write(dst, base)?;
                Ok(Step::Next)
            }
            Instr::GetSuper {
                dst,
                base,
                key,
                receiver,
            } => {
                let ValueRef::Object(base) = frame.read_ref(base)?.view() else {
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
                let ValueRef::Object(base) = frame.read_ref(base)?.view() else {
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
                frame.write(dst, Value::object(env))?;
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
                name_key,
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
                        Some(parent) if matches!(parent.view(), ValueRef::Null) => {
                            (function_root, None)
                        }
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
                                Invocation::Value(value) => {
                                    match value.view() {
                                        ValueRef::Object(id) => (parent_id, Some(id)),
                                        ValueRef::Null => (parent_id, None),
                                        _ => return Ok(Step::Throw(self.type_error(
                                            "class heritage prototype must be an object or null",
                                        )?)),
                                    }
                                }
                            }
                        }
                    };
                let dynamic_name = name_key
                    .map(|slot| {
                        Ok::<_, VmError>(self.property_function_name(
                            converted_property_key(frame.read_ref(slot)?)?,
                            None,
                        ))
                    })
                    .transpose()?;
                let class = self.heap.alloc_function(constructor, env);
                let prototype = self.heap.alloc_plain_with(instance_parent);
                let Object::Function(function) = self.heap.get_mut(class)? else {
                    unreachable!()
                };
                function.prototype = Some(static_parent);
                function.dynamic_name = dynamic_name;
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
                self.install_script_metadata(class, constructor)?;
                self.heap.define_data(
                    class,
                    "prototype",
                    Value::object(prototype),
                    Attributes::FIXED,
                )?;
                self.heap.define_data(
                    prototype,
                    "constructor",
                    Value::object(class),
                    Attributes::METHOD,
                )?;
                frame.write(dst, Value::object(class))?;
                Ok(Step::Next)
            }
            Instr::ScopedClosure {
                dst,
                scope,
                function,
            } => {
                let env = self.class_scope(frame, Some(scope))?;
                let closure = self.materialize_function(function, env)?;
                frame.write(dst, Value::object(closure))?;
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
                let ValueRef::Object(object) = frame.read_ref(object)?.view() else {
                    return Ok(Step::Throw(
                        self.type_error("private brand check requires an object")?,
                    ));
                };
                let present = self
                    .heap
                    .private
                    .get(&object)
                    .is_some_and(|elements| elements.contains_key(&(brand, index)));
                frame.write(dst, Value::boolean(present))?;
                Ok(Step::Next)
            }
            Instr::NewObject { dst } => {
                let id = self.heap.alloc_plain();
                frame.write(dst, Value::object(id))?;
                Ok(Step::Next)
            }
            Instr::NewRegExp { dst, source, flags } => {
                let id = self.alloc_regexp(&source, &flags)?;
                frame.write(dst, Value::object(id))?;
                Ok(Step::Next)
            }
            Instr::NewArray { dst, elements } => {
                let elements = frame.read_list(&elements)?;
                let id = self.heap.alloc_array(elements);
                frame.write(dst, Value::object(id))?;
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
                if matches!(&invocation, Invocation::Value(value)
                    if matches!(value.view(), ValueRef::Bool(false)))
                {
                    return Ok(Step::Throw(
                        self.type_error("cannot delete a non-configurable property")?,
                    ));
                }
                store_invocation(frame, dst, invocation)
            }
            Instr::GetForInIterator { dst, source } => {
                let source = frame.read_ref(source)?;
                let prototype = self.primitive_prototype(source);
                let iterator = self.heap.alloc_for_in_iterator(source, prototype);
                frame.write(dst, Value::object(iterator))?;
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
                frame.write(done, Value::boolean(key.is_none()))?;
                frame.write(value, key.map_or(Value::UNDEFINED, Value::string))?;
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
            Instr::IteratorCall {
                dst,
                iterator,
                method,
                argument,
            } => {
                let iterator = frame.read(iterator)?;
                let method = frame.read(method)?;
                let args = match argument {
                    Some(argument) => vec![frame.read(argument)?],
                    None => Vec::new(),
                };
                let invocation = self.invoke(host, frame_roots, &method, args, &iterator)?;
                let result = match self
                    .require_iterator_object(invocation, "iterator method must return an object")?
                {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                };
                frame.write(dst, result)?;
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
                frame.write(done, Value::boolean(item.is_none()))?;
                frame.write(value, item.unwrap_or(Value::UNDEFINED))?;
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
                frame.write(done, Value::boolean(done_value))?;
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
            Instr::GetTemplate { dst, template } => {
                let value = self.template_object(template)?;
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
                if matches!(
                    frame.read_ref(src)?.view(),
                    ValueRef::Null | ValueRef::Undefined
                ) {
                    return Ok(Step::Throw(
                        self.type_error("Cannot destructure null or undefined")?,
                    ));
                }
                Ok(Step::Next)
            }
            Instr::RequireObject { src } => {
                if matches!(frame.read_ref(src)?.view(), ValueRef::Object(_)) {
                    Ok(Step::Next)
                } else {
                    Ok(Step::Throw(
                        self.type_error("async iterator must be an object")?,
                    ))
                }
            }
            Instr::ObjectRest { dst, src, excluded } => {
                let source = frame.read_ref(src)?;
                if matches!(source.view(), ValueRef::Null | ValueRef::Undefined) {
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
                    Invocation::Value(_) => frame.write(dst, Value::object(target))?,
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
            Instr::SaveEnv { dst } => {
                let env = frame.env.ok_or_else(|| {
                    VmError::InvalidState("loop without an invocation environment".to_owned())
                })?;
                frame.write(dst, Value::object(env))?;
                Ok(Step::Next)
            }
            Instr::NewIterationEnv { parent, bindings } => {
                let parent = object_id(&frame.read(parent)?, "iteration environment parent")?;
                if !matches!(self.heap.get(parent)?, Object::Environment(_)) {
                    return Err(VmError::InvalidState(
                        "iteration environment parent is not an environment".to_owned(),
                    ));
                }
                let current = frame.env.ok_or_else(|| {
                    VmError::InvalidState("iteration environment without a current env".to_owned())
                })?;
                let mut values = BTreeMap::new();
                for binding in bindings {
                    values.insert(binding, self.heap.get_binding(current, binding)?);
                }
                frame.env = Some(self.heap.alloc_environment(Some(parent), values));
                Ok(Step::Next)
            }
            Instr::RestoreEnv { src } => {
                let env = object_id(&frame.read(src)?, "restored environment")?;
                if !matches!(self.heap.get(env)?, Object::Environment(_)) {
                    return Err(VmError::InvalidState(
                        "restored loop environment is not an environment".to_owned(),
                    ));
                }
                frame.env = Some(env);
                Ok(Step::Next)
            }
            Instr::Await { input, resume, .. } => self.suspend(host, roots, frame, input, resume),
            Instr::Yield {
                input,
                generator,
                delegate,
            } => self.suspend_generator(frame, input, generator, delegate),
            Instr::GeneratorStart => self.start_generator(host, roots, frame),
            Instr::LoadGeneratorReturn { dst } => {
                let value = frame.generator_return.take().ok_or_else(|| {
                    VmError::InvalidState(
                        "generator return payload is missing for this resume".to_owned(),
                    )
                })?;
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
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
            None => Value::UNDEFINED,
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
        let method = if matches!(iterable.view(), ValueRef::Object(_) | ValueRef::String(_)) {
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
        if !matches!(iterator.view(), ValueRef::Object(_)) {
            return self.throw_type_error("iterator must be an object");
        }
        let roots = &roots.with(iterator);
        let method =
            match self.read_property(host, roots, iterator, PropertyKeyRef::String("return"))? {
                Invocation::Value(value)
                    if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
                {
                    return Ok(Invocation::Value(Value::UNDEFINED));
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
            Ok(string) => Invocation::Value(Value::string(string)),
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
        if matches!(value.as_ref().view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(ops::SYMBOL_TO_STRING)?));
        }
        Ok(Ok(ops::to_js_string(&value)))
    }

    /// `Number(value)` accepts BigInt by an explicit precision-losing conversion,
    /// unlike arithmetic and `Number.prototype` methods.
    pub(crate) fn coerce_number_allow_bigint(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::Number)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if let ValueRef::BigInt(value) = value.as_ref().view() {
            return Ok(Ok(value.to_f64()));
        }
        if matches!(value.as_ref().view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(ops::SYMBOL_TO_NUMBER)?));
        }
        Ok(Ok(ops::to_number(&value)?))
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
        if matches!(value.as_ref().view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(ops::SYMBOL_TO_NUMBER)?));
        }
        if matches!(value.as_ref().view(), ValueRef::BigInt(_)) {
            return Ok(Err(self.type_error(ops::BIGINT_TO_NUMBER)?));
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
        Ok(Ok(match value.as_ref().view() {
            ValueRef::Symbol(id) => PropertyKey::Symbol(id),
            _ => PropertyKey::String(ops::to_js_string(&value)),
        }))
    }

    pub(crate) fn coerce_primitive<'a>(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &'a Value,
        hint: Hint,
    ) -> Result<Result<Cow<'a, Value>, Value>, VmError> {
        if !matches!(value.view(), ValueRef::Object(_)) {
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
            Invocation::Value(value)
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) => {}
            Invocation::Value(method) => {
                let result = self.invoke(
                    host,
                    roots,
                    &method,
                    vec![Value::string(hint.as_str().into())],
                    value,
                )?;
                return match result {
                    Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {
                        self.throw_type_error("Cannot convert object to primitive value")
                    }
                    result => Ok(result),
                };
            }
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        }
        self.ordinary_to_primitive(host, roots, value, hint)
    }

    pub(crate) fn ordinary_to_primitive(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        hint: Hint,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(value);
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
                Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {}
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
        match &invocation {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {
                Ok(invocation)
            }
            Invocation::Thrown(_) => Ok(invocation),
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
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("property access on a non-object");
        }
        match key.view() {
            ValueRef::String(key) => {
                return self.read_property(host, roots, object, PropertyKeyRef::JsString(key));
            }
            ValueRef::Symbol(id) => {
                return self.read_property(host, roots, object, PropertyKeyRef::Symbol(id));
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
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
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
        if let ValueRef::String(text) = object.view()
            && let Some(property) = string_own_property(text, key)
        {
            return self.read_property_value(host, roots, object, property.into_owned());
        }
        if let Some(prototype) = self.primitive_prototype(object) {
            return self.read_property_with_receiver(host, roots, prototype, object, key);
        }
        let ValueRef::Object(id) = object.view() else {
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
        self.read_property_from_chain(host, roots, target, key, |_| Cow::Borrowed(receiver))
    }

    /// Get on a ToObject result. Keep the same wrapper across accesses, but
    /// defer creating it until an accessor or proxy can observe the receiver.
    pub(crate) fn read_to_object_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        boxed: &mut Option<ObjectId>,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        if let Some(id) = *boxed {
            return self.read_property(host, &roots.with(&id), &Value::object(id), key);
        }
        if let ValueRef::Object(_) = value.view() {
            return self.read_property(host, roots, value, key);
        }
        if let ValueRef::String(text) = value.view()
            && let Some(property) = string_own_property(text, key)
        {
            return self.read_property_value(host, roots, value, property.into_owned());
        }
        let prototype = self
            .primitive_prototype(value)
            .expect("validated ToObject operand");
        self.read_property_from_chain(host, roots, prototype, key, |vm| {
            let id = vm.coercible_object(value);
            *boxed = Some(id);
            Cow::Owned(Value::object(id))
        })
    }

    fn read_property_from_chain<'a>(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKeyRef<'_>,
        receiver: impl FnOnce(&mut Self) -> Cow<'a, Value>,
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
                return Ok(Invocation::Value(Value::object(
                    self.host_classes[class].prototype,
                )));
            }
            if let Object::Proxy {
                target, handler, ..
            } = *object
            {
                let receiver = receiver(self);
                return self.proxy_get(host, roots, target, handler, key, &receiver);
            }
            // Integer-indexed exotic [[Get]]: a canonical index never falls
            // through to the prototype chain, even when it addresses nothing.
            match self.heap.typed_index(id, key)? {
                TypedIndex::Invalid => return Ok(Invocation::Value(Value::UNDEFINED)),
                TypedIndex::Valid(_) | TypedIndex::Ordinary => {}
            }
            if let Some(property) = self.heap.own_property(id, key)? {
                return match property.into_owned() {
                    Property::Data(value) => Ok(Invocation::Value(value)),
                    Property::Accessor { get: Some(get), .. } => {
                        let receiver = receiver(self);
                        self.invoke(host, roots, &Value::object(get), Vec::new(), &receiver)
                    }
                    Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::UNDEFINED)),
                };
            }
            current = object.prototype();
        }
        Ok(Invocation::Value(Value::UNDEFINED))
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
        let receiver = Value::object(id);
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
                self.invoke(host, roots, &Value::object(get), Vec::new(), receiver)
            }
            Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::UNDEFINED)),
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
        let ValueRef::Object(mut target) = constructor.view() else {
            return if ordinary {
                Ok(Invocation::Value(Value::boolean(false)))
            } else {
                self.throw_type_error("right-hand side of 'instanceof' is not an object")
            };
        };
        let live = (value, constructor);
        let roots = &roots.with(&live);
        let mut remaining = self.heap.object_count();
        loop {
            let receiver = Value::object(target);
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
                if !matches!(hook.view(), ValueRef::Undefined | ValueRef::Null) {
                    if !self.is_callable(&hook) {
                        return self.throw_type_error("Symbol.hasInstance is not callable");
                    }
                    // Inline the default intrinsic after lookup so a chain of
                    // bound functions does not recurse through native calls.
                    let ValueRef::Object(hook_id) = hook.view() else {
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
                                    Invocation::Value(Value::boolean(result.truthy()))
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
                    Ok(Invocation::Value(Value::boolean(false)))
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

        let ValueRef::Object(mut current) = value.view() else {
            return Ok(Invocation::Value(Value::boolean(false)));
        };
        let prototype = match self.read_property(
            host,
            roots,
            &Value::object(target),
            PropertyKeyRef::String("prototype"),
        )? {
            Invocation::Value(value) => {
                let ValueRef::Object(id) = value.view() else {
                    return self
                        .throw_type_error("'instanceof' constructor prototype is not an object");
                };
                id
            }
            Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
        };
        // Prototype links are kept acyclic by set_prototype and snapshot restore.
        while let Some(parent) = self.heap.prototype_of(current)? {
            if parent == prototype {
                return Ok(Invocation::Value(Value::boolean(true)));
            }
            current = parent;
        }
        Ok(Invocation::Value(Value::boolean(false)))
    }

    fn has_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = object.view() else {
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
            match self.heap.typed_index(id, key)? {
                TypedIndex::Valid(_) => return Ok(Invocation::Value(Value::boolean(true))),
                TypedIndex::Invalid => return Ok(Invocation::Value(Value::boolean(false))),
                TypedIndex::Ordinary => {}
            }
            if self.heap.own_property(id, key)?.is_some() {
                return Ok(Invocation::Value(Value::boolean(true)));
            }
            current = object.prototype();
        }
        Ok(Invocation::Value(Value::boolean(false)))
    }

    pub(crate) fn delete_property(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("cannot delete a property of null or undefined");
        }
        let roots = &roots.with(target);
        let coerced;
        let key = match key.view() {
            ValueRef::String(key) => PropertyKeyRef::JsString(key),
            ValueRef::Symbol(id) => PropertyKeyRef::Symbol(id),
            _ => {
                coerced = match self.coerce_property_key(host, roots, key)? {
                    Ok(key) => key,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                coerced.borrowed()
            }
        };
        if let ValueRef::Object(id) = target.view()
            && let Object::Proxy {
                target, handler, ..
            } = *self.heap.get(id)?
        {
            return self.proxy_presence(host, roots, target, handler, key, true);
        }
        let deleted = match (target.view(), key) {
            (ValueRef::Object(id), key) => self.heap.delete_own(id, key)?,
            // ToObject on a string would expose fixed UTF-16 indices and length.
            (ValueRef::String(string), key) => {
                !key.is_str("length")
                    && !ops::canonical_index_key(key).is_some_and(|index| index < string.unit_len())
            }
            _ => true,
        };
        Ok(Invocation::Value(Value::boolean(deleted)))
    }

    fn write_member(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("property write on a non-object");
        }
        let roots = &roots.with(object);
        let key = match self.coerce_property_key(host, &roots.with(&value), key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let id = match object.view() {
            ValueRef::Object(id) => id,
            _ => {
                let key = key.borrowed();
                if let ValueRef::String(text) = object.view()
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
        let success = Invocation::Value(Value::boolean(true));
        let reject = |vm: &mut Self, message: &str| {
            if strict {
                vm.throw_type_error(message)
            } else {
                Ok(Invocation::Value(Value::boolean(false)))
            }
        };

        // Integer-indexed exotic [[Set]]: a canonical index is resolved before
        // any prototype lookup, and the value still converts when the index
        // addresses no element.
        match self.heap.typed_index(target, key.borrowed())? {
            TypedIndex::Ordinary => {}
            TypedIndex::Valid(index) => {
                if ops::strict_equal(receiver, &Value::object(target)) {
                    return match self.typed_set_element(host, roots, target, index, &value)? {
                        Ok(()) => Ok(success),
                        Err(error) => Ok(Invocation::Thrown(error)),
                    };
                }
            }
            TypedIndex::Invalid => {
                // The conversion of the value is observable; the store is not.
                if ops::strict_equal(receiver, &Value::object(target))
                    && let Some(kind) = self.heap.typed_array(target)?.map(|view| view.kind)
                    && let Err(error) = self.element_bits(host, roots, kind, &value)?
                {
                    return Ok(Invocation::Thrown(error));
                }
                return Ok(success);
            }
        }

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
            property = self.heap.own_property(id, key.borrowed())?;
            if property.is_some() {
                break;
            }
            current = object.prototype();
        }
        match property.as_ref().and_then(OwnProperty::setter) {
            Some(Some(set)) => {
                return match self.invoke(host, roots, &Value::object(set), vec![value], receiver)? {
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

        let ValueRef::Object(id) = receiver.view() else {
            return reject(
                self,
                "Cannot write a data property on a non-object receiver",
            );
        };
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
            match self.heap.own_property(id, key.borrowed())? {
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
        match source.view() {
            ValueRef::Object(id) => {
                // Capture the order once; an earlier getter may delete a later key
                // or change its enumerability before it is visited.
                let keys = match self.own_keys(host, roots, id, false)? {
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
                    match self.own_descriptor(host, key_roots, id, key.borrowed())? {
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
            ValueRef::String(text) => {
                for (index, unit) in text.units().enumerate() {
                    let key = index.to_string();
                    if excluded.contains(&PropertyKeyRef::String(&key)) {
                        continue;
                    }
                    self.heap.define_property(
                        target,
                        PropertyKey::String(key.into()),
                        Property::Data(Value::string(utf16_unit_string(unit))),
                    )?;
                }
            }
            // Nullish values and the remaining primitive wrappers have no keys to copy.
            _ => {}
        }
        Ok(Invocation::Value(Value::UNDEFINED))
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
                // Only field initializers with a dynamic name take a key parameter.
                let needs_key = key.is_some()
                    && self
                        .heap
                        .function(closure)
                        .and_then(|function| self.build.function(function.func_id))
                        .is_some_and(|code| !code.params.is_empty());
                match self.invoke(
                    host,
                    &roots.with(&key),
                    &Value::object(closure),
                    key.iter()
                        .filter(|_| needs_key)
                        .map(|key| match key {
                            PropertyKey::String(key) => Value::string(key.clone()),
                            PropertyKey::Symbol(key) => Value::symbol(*key),
                        })
                        .collect(),
                    &Value::object(receiver),
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Some(error)),
                }
            } else {
                Value::UNDEFINED
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

    fn property_function_name(
        &self,
        key: PropertyKeyRef<'_>,
        prefix: Option<AccessorPrefix>,
    ) -> JsString {
        let mut name = JsString::from(match prefix {
            Some(AccessorPrefix::Get) => "get ",
            Some(AccessorPrefix::Set) => "set ",
            None => "",
        });
        match key {
            PropertyKeyRef::String(key) => name.push_str(&key.into()),
            PropertyKeyRef::JsString(key) => name.push_str(key),
            PropertyKeyRef::Symbol(symbol) => {
                if let Some(Some(description)) = self.heap.symbols.get(&symbol) {
                    name.push_str(&"[".into());
                    name.push_str(description);
                    name.push_str(&"]".into());
                }
            }
        }
        name
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
        let Object::Function(function) = self.heap.get_mut(src)? else {
            unreachable!()
        };
        function.home = Some(target);
        let mut update = DescriptorUpdate {
            enumerable: Some(false),
            configurable: Some(true),
            ..Default::default()
        };
        match kind {
            PropertyDefinition::Data => {
                update.value = Some(Value::object(src));
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
        frame.write(dst, Value::object(id))?;
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
        let target = frame
            .target
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
            AwaitPrepared::Promise(id) => (Some(id), Value::UNDEFINED),
            AwaitPrepared::Plain(value) => (None, value),
        };
        let continuation_id = self.alloc_continuation();
        self.continuations.insert(
            continuation_id,
            Continuation {
                target,
                func_id,
                resume_id: resume,
                waiting_on,
                env: frame.env,
                values,
            },
        );
        if let ContinuationTarget::Generator(id) = target {
            // The frame now lives in the continuation; the generator object
            // only records that its body is waiting.
            let record = self.heap.generator_mut(id).ok_or_else(|| {
                VmError::InvalidState(format!("generator object {id} is missing"))
            })?;
            record.status = crate::heap::GeneratorStatus::Awaiting;
            record.env = None;
            record.values.clear();
        }
        self.schedule_resume(continuation_id, waiting_on, plain)?;
        Ok(Step::Suspend)
    }

    /// Capture the suspended frame into the generator object that owns it.
    ///
    /// The object was marked `Executing` by the resume that built this frame, so
    /// the three entry destinations are irrelevant here: every value the entry
    /// needs that is not injected is in `saved_slots`.
    fn suspend_generator(
        &mut self,
        frame: &Frame,
        input: Slot,
        generator: u32,
        delegate: bool,
    ) -> Result<Step, VmError> {
        let value = frame.read(input)?;
        let id = frame
            .generator
            .ok_or_else(|| VmError::InvalidState("Yield outside a generator frame".to_owned()))?;
        let entry = self.build.functions[frame.function]
            .generator_resumes
            .get(generator as usize)
            .cloned()
            .ok_or_else(|| {
                VmError::InvalidState(format!("generator resume {generator} does not exist"))
            })?;
        let values = frame.read_saved(&entry.saved_slots)?;
        let record = self
            .heap
            .generator_mut(id)
            .ok_or_else(|| VmError::InvalidState(format!("generator object {id} is missing")))?;
        record.status = crate::heap::GeneratorStatus::Suspended(generator);
        record.env = frame.env;
        record.values = values;
        Ok(Step::Yield { value, delegate })
    }

    /// Build the generator object the call returns, without running the body.
    ///
    /// The instance prototype is the function's `.prototype` when that is an
    /// object and the shared generator prototype otherwise.
    fn start_generator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
    ) -> Result<Step, VmError> {
        let function = &self.build.functions[frame.function];
        let func_id = function.func_id;
        let shared = if function.is_async {
            self.heap.async_generator_prototype
        } else {
            self.heap.generator_prototype
        };
        let start = function.generator_start.clone().ok_or_else(|| {
            VmError::InvalidState("GeneratorStart outside a generator".to_owned())
        })?;
        let values = frame.read_saved(&start.saved_slots)?;
        let prototype = match frame.callee {
            Some(callee) => {
                let callee = Value::object(callee);
                let roots = &roots.with(&callee);
                let read =
                    self.read_property(host, roots, &callee, PropertyKeyRef::String("prototype"))?;
                match read {
                    Invocation::Value(value) => match value.view() {
                        ValueRef::Object(id) => Some(id),
                        _ => shared,
                    },
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                }
            }
            None => shared,
        };
        let id = self.heap.alloc_generator(
            prototype.or(shared),
            crate::heap::GeneratorObject {
                func_id,
                status: crate::heap::GeneratorStatus::Start,
                env: frame.env,
                values,
                requests: std::collections::VecDeque::new(),
            },
        );
        Ok(Step::Return(Value::object(id)))
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

    fn unary(&self, operator: UnaryOp, value: Value) -> Result<Value, BinaryError> {
        if matches!(operator, UnaryOp::TypeOf) && self.is_callable(&value) {
            return Ok(Value::string("function".into()));
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
            crate::bytecode::Constant::Null => Value::NULL,
            crate::bytecode::Constant::Bool(value) => Value::boolean(*value),
            crate::bytecode::Constant::Number(value) => Value::number(*value),
            crate::bytecode::Constant::String(value) => Value::string(value.clone()),
            crate::bytecode::Constant::BigInt(value) => Value::bigint(value.clone()),
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
    match value.view() {
        ValueRef::String(key) => Ok(PropertyKeyRef::JsString(key)),
        ValueRef::Symbol(id) => Ok(PropertyKeyRef::Symbol(id)),
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

/// Which externally injected completion resumes a suspended generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeneratorCompletion {
    Next,
    Throw,
    Return,
}

impl Vm {
    /// Queue one request for an async generator method call.
    ///
    /// A receiver that is not an async generator still returns a Promise: the
    /// method rejects with a `TypeError` instead of throwing synchronously.
    pub(crate) fn resume_async_generator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Object(id) => id,
            _ => return self.reject_type_error("receiver is not an async generator"),
        };
        let Some(record) = self.heap.generator(id) else {
            return self.reject_type_error("receiver is not an async generator");
        };
        let func_id = record.func_id;
        if !self.is_async_generator(func_id)? {
            return self.reject_type_error("receiver is not an async generator");
        }
        self.enqueue_async_request(host, roots, id, completion, argument)
    }

    /// A fresh Promise already rejected with a script `TypeError`.
    fn reject_type_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        let error = self.type_error(message)?;
        let promise = self.heap.alloc_promise();
        self.settle(promise, Settlement::Rejected(error))?;
        Ok(Invocation::Value(Value::object(promise)))
    }

    /// Resume (or start, or close) a generator object.
    ///
    /// The suspended frame is taken out of the object and the object is marked
    /// `Executing` first, so a reentrant resume from inside the body throws a
    /// script TypeError instead of observing a half-taken frame. Only the
    /// persistent slots in the entry's `saved_slots` are restored; every other
    /// slot keeps the "uninitialized" state it must have had at the suspension.
    ///
    /// An async generator never runs the body here: the completion becomes a
    /// queued request, so the body may already be executing or awaiting.
    pub(crate) fn resume_generator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Object(id) => id,
            _ => return self.throw_type_error("generator method called on a non-object"),
        };
        let Some(record) = self.heap.generator(id).cloned() else {
            return self.throw_type_error("receiver is not a generator");
        };
        if self.is_async_generator(record.func_id)? {
            return self.enqueue_async_request(host, roots, id, completion, argument);
        }
        if matches!(
            record.status,
            GeneratorStatus::Awaiting | GeneratorStatus::Closing
        ) {
            return Err(VmError::InvalidState(format!(
                "synchronous generator {id} is in an async waiting state"
            )));
        }
        if matches!(record.status, GeneratorStatus::Executing) {
            return self.throw_type_error("generator is already executing");
        }
        if matches!(record.status, GeneratorStatus::Completed) {
            return match completion {
                GeneratorCompletion::Next => self.iterator_result(Value::UNDEFINED, true),
                GeneratorCompletion::Throw => Ok(Invocation::Thrown(argument)),
                GeneratorCompletion::Return => self.iterator_result(argument, true),
            };
        }
        let outcome = self.enter_generator(id, &record, completion, argument.clone())?;
        let Some(frame) = outcome else {
            // Nothing ran yet, so the body is never entered: the generator
            // completes with the injected completion itself.
            return match completion {
                GeneratorCompletion::Next => Err(VmError::InvalidState(
                    "a generator body cannot stay unentered for next".to_owned(),
                )),
                GeneratorCompletion::Throw => Ok(Invocation::Thrown(argument)),
                GeneratorCompletion::Return => self.iterator_result(argument, true),
            };
        };
        let outcome = self.execute_frame(host, roots, frame)?;
        self.finish_generator(id, outcome)
    }

    fn is_async_generator(&self, func_id: u32) -> Result<bool, VmError> {
        let function = self
            .build
            .function(func_id)
            .ok_or_else(|| VmError::InvalidState(format!("function {func_id} does not exist")))?;
        Ok(function.is_generator && function.is_async)
    }

    /// Restore the frame for one externally injected completion.
    ///
    /// `None` means the body must never run: a completion other than `next`
    /// arrived before the first `next`. The generator is completed here so the
    /// caller reports the completion itself.
    fn enter_generator(
        &mut self,
        id: ObjectId,
        record: &crate::heap::GeneratorObject,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Option<Frame>, VmError> {
        let function_index = self.function_index(record.func_id)?;
        let target = self.build.functions[function_index]
            .is_async
            .then_some(ContinuationTarget::Generator(id));
        match record.status {
            GeneratorStatus::Start => {
                if completion != GeneratorCompletion::Next {
                    self.complete_generator(id)?;
                    return Ok(None);
                }
                let start = self.build.functions[function_index]
                    .generator_start
                    .clone()
                    .ok_or_else(|| {
                        VmError::InvalidState(format!("{} is not a generator", record.func_id))
                    })?;
                self.mark_generator_executing(id)?;
                let mut frame = Frame::new(
                    function_index,
                    &self.build.functions[function_index],
                    record.env,
                    target,
                );
                self.restore_saved(&mut frame, &start.saved_slots, &record.values)?;
                frame.pc = start.body_pc as usize;
                frame.generator = Some(id);
                Ok(Some(frame))
            }
            GeneratorStatus::Suspended(index) => {
                let entry = self.build.functions[function_index]
                    .generator_resumes
                    .get(index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        VmError::InvalidState(format!("generator resume {index} does not exist"))
                    })?;
                self.mark_generator_executing(id)?;
                let mut frame = Frame::new(
                    function_index,
                    &self.build.functions[function_index],
                    record.env,
                    target,
                );
                self.restore_saved(&mut frame, &entry.saved_slots, &record.values)?;
                match completion {
                    GeneratorCompletion::Next => {
                        frame.write(entry.next_dst, argument)?;
                        frame.pc = entry.next_pc as usize;
                    }
                    GeneratorCompletion::Throw => {
                        frame.write(entry.throw_dst, argument)?;
                        frame.pc = entry.throw_pc as usize;
                    }
                    GeneratorCompletion::Return => {
                        frame.generator_return = Some(argument);
                        frame.pc = entry.return_pc as usize;
                    }
                }
                frame.generator = Some(id);
                Ok(Some(frame))
            }
            _ => Err(VmError::InvalidState(format!(
                "generator {id} cannot run its body while {:?}",
                record.status
            ))),
        }
    }

    /// Queue one async generator request and drive the generator as far as the
    /// queue allows. The request Promise is always pending until a step settles
    /// it, so a caller inside the body only appends.
    fn enqueue_async_request(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let promise = self.heap.alloc_promise();
        let record = self
            .heap
            .generator_mut(id)
            .ok_or_else(|| VmError::InvalidState(format!("generator object {id} is missing")))?;
        record.requests.push_back(AsyncRequest {
            completion: match completion {
                GeneratorCompletion::Next => AsyncCompletion::Next,
                GeneratorCompletion::Throw => AsyncCompletion::Throw,
                GeneratorCompletion::Return => AsyncCompletion::Return,
            },
            argument,
            promise,
        });
        self.drive_async_generator(host, roots, id)?;
        Ok(Invocation::Value(Value::object(promise)))
    }

    /// Drain the async generator queue one step at a time. Each step runs the
    /// body at most once or settles the head request, never both, so the queue
    /// is not emptied by recursion.
    fn drive_async_generator(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<(), VmError> {
        while self.async_generator_step(host, roots, id)? {}
        Ok(())
    }

    fn async_generator_step(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<bool, VmError> {
        let Some(record) = self.heap.generator(id).cloned() else {
            return Err(VmError::InvalidState(format!(
                "generator object {id} is missing"
            )));
        };
        if matches!(
            record.status,
            GeneratorStatus::Executing | GeneratorStatus::Awaiting | GeneratorStatus::Closing
        ) {
            return Ok(false);
        }
        let Some(head) = record.requests.front().cloned() else {
            return Ok(false);
        };
        match record.status {
            GeneratorStatus::Completed => {
                self.settle_finished_request(host, roots, id, &head)?;
                Ok(true)
            }
            GeneratorStatus::Start | GeneratorStatus::Suspended(_) => {
                let completion = match head.completion {
                    AsyncCompletion::Next => GeneratorCompletion::Next,
                    AsyncCompletion::Throw => GeneratorCompletion::Throw,
                    AsyncCompletion::Return => GeneratorCompletion::Return,
                };
                let Some(frame) =
                    self.enter_generator(id, &record, completion, head.argument.clone())?
                else {
                    // A completion before the first `next` never enters the
                    // body; the completed branch settles it on the next step.
                    return Ok(true);
                };
                let outcome = self.execute_frame(host, roots, frame)?;
                self.finish_async_generator_body(id, outcome)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Settle one request against a generator whose body can no longer run.
    ///
    /// `next` reports `done`, `throw` rejects, and `return` assimilates its
    /// argument through a persisted Promise reaction before reporting `done`.
    fn settle_finished_request(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        head: &AsyncRequest,
    ) -> Result<(), VmError> {
        match head.completion {
            AsyncCompletion::Next => {
                let result = self.iterator_result_value(Value::UNDEFINED, true)?;
                self.settle(head.promise, Settlement::Fulfilled(result))?;
                self.pop_async_request(id, head.promise)?;
            }
            AsyncCompletion::Throw => {
                self.settle(head.promise, Settlement::Rejected(head.argument.clone()))?;
                self.pop_async_request(id, head.promise)?;
            }
            AsyncCompletion::Return => {
                match self.prepare_await(
                    host,
                    &roots.with(&head.argument),
                    head.argument.clone(),
                )? {
                    Ok(AwaitPrepared::Plain(value)) => {
                        let result = self.iterator_result_value(value, true)?;
                        self.settle(head.promise, Settlement::Fulfilled(result))?;
                        self.pop_async_request(id, head.promise)?;
                    }
                    Ok(AwaitPrepared::Promise(promise)) => {
                        self.react(
                            promise,
                            crate::heap::ReactionJob::GeneratorClose {
                                generator: id,
                                request: head.promise,
                            },
                        )?;
                        let record = self.heap.generator_mut(id).ok_or_else(|| {
                            VmError::InvalidState(format!("generator object {id} is missing"))
                        })?;
                        record.status = GeneratorStatus::Closing;
                    }
                    Err(error) => {
                        // Reading the argument's `then` threw: this request
                        // alone is rejected, the generator stays completed.
                        self.settle(head.promise, Settlement::Rejected(error))?;
                        self.pop_async_request(id, head.promise)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Settle the request whose return argument finished assimilating.
    pub(crate) fn finish_closing_request(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        generator: ObjectId,
        request: ObjectId,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let record = self.heap.generator(generator).ok_or_else(|| {
            VmError::InvalidState(format!("generator object {generator} is missing"))
        })?;
        if record.status != GeneratorStatus::Closing {
            return Err(VmError::InvalidState(format!(
                "generator {generator} is not closing a request"
            )));
        }
        match settlement {
            Settlement::Fulfilled(value) => {
                let result = self.iterator_result_value(value.clone(), true)?;
                self.settle(request, Settlement::Fulfilled(result))?;
            }
            Settlement::Rejected(reason) => {
                self.settle(request, Settlement::Rejected(reason.clone()))?;
            }
        }
        self.complete_generator(generator)?;
        self.pop_async_request(generator, request)?;
        self.drive_async_generator(host, roots, generator)
    }

    fn head_async_request(&self, id: ObjectId) -> Result<AsyncRequest, VmError> {
        self.heap
            .generator(id)
            .and_then(|record| record.requests.front().cloned())
            .ok_or_else(|| {
                VmError::InvalidState(format!("async generator {id} has no active request"))
            })
    }

    fn pop_async_request(&mut self, id: ObjectId, promise: ObjectId) -> Result<(), VmError> {
        let record = self
            .heap
            .generator_mut(id)
            .ok_or_else(|| VmError::InvalidState(format!("generator object {id} is missing")))?;
        match record.requests.pop_front() {
            Some(request) if request.promise == promise => Ok(()),
            _ => Err(VmError::InvalidState(format!(
                "async generator {id} head is not request {promise}"
            ))),
        }
    }

    fn mark_generator_executing(&mut self, id: ObjectId) -> Result<(), VmError> {
        let record = self
            .heap
            .generator_mut(id)
            .ok_or_else(|| VmError::InvalidState(format!("generator object {id} is missing")))?;
        record.status = GeneratorStatus::Executing;
        Ok(())
    }

    /// Drop the suspended frame once the body can never run again.
    fn complete_generator(&mut self, id: ObjectId) -> Result<(), VmError> {
        let record = self
            .heap
            .generator_mut(id)
            .ok_or_else(|| VmError::InvalidState(format!("generator object {id} is missing")))?;
        record.status = GeneratorStatus::Completed;
        record.env = None;
        record.values.clear();
        Ok(())
    }

    fn restore_saved(
        &self,
        frame: &mut Frame,
        slots: &[u16],
        values: &[Value],
    ) -> Result<(), VmError> {
        if slots.len() != values.len() {
            return Err(VmError::InvalidState(format!(
                "generator holds {} saved values, expected {}",
                values.len(),
                slots.len()
            )));
        }
        for (slot, value) in slots.iter().zip(values) {
            frame.write(
                Slot {
                    kind: SlotKind::Persistent,
                    index: *slot,
                },
                value.clone(),
            )?;
        }
        Ok(())
    }

    fn finish_generator(
        &mut self,
        id: ObjectId,
        outcome: ExecOutcome,
    ) -> Result<Invocation, VmError> {
        match outcome {
            // The suspension already recorded itself in the generator object.
            ExecOutcome::Yielded { value, delegate } => {
                if delegate {
                    Ok(Invocation::Value(value))
                } else {
                    self.iterator_result(value, false)
                }
            }
            ExecOutcome::Return(value) => {
                self.complete_generator(id)?;
                self.iterator_result(value, true)
            }
            ExecOutcome::Thrown(value) => {
                self.complete_generator(id)?;
                Ok(Invocation::Thrown(value))
            }
            ExecOutcome::Suspended => Err(VmError::InvalidState(
                "generator awaited inside a synchronous generator".to_owned(),
            )),
        }
    }

    /// Settle the in-progress request from how one body step finished.
    ///
    /// A yield fulfils it with `done: false`, a return with `done: true`, and an
    /// uncaught script throw rejects it and closes the generator. An `await`
    /// leaves the request in place: the continuation resumes this body, not a
    /// fresh request.
    fn finish_async_generator_body(
        &mut self,
        id: ObjectId,
        outcome: ExecOutcome,
    ) -> Result<(), VmError> {
        let head = self.head_async_request(id)?;
        match outcome {
            // `suspend` already marked the generator `Awaiting`.
            ExecOutcome::Suspended => Ok(()),
            ExecOutcome::Yielded { value, delegate } => {
                if delegate {
                    return Err(VmError::InvalidState(
                        "async generator yielded a delegate result object".to_owned(),
                    ));
                }
                let result = self.iterator_result_value(value, false)?;
                self.settle(head.promise, Settlement::Fulfilled(result))?;
                self.pop_async_request(id, head.promise)
            }
            ExecOutcome::Return(value) => {
                self.complete_generator(id)?;
                let result = self.iterator_result_value(value, true)?;
                self.settle(head.promise, Settlement::Fulfilled(result))?;
                self.pop_async_request(id, head.promise)
            }
            ExecOutcome::Thrown(value) => {
                self.complete_generator(id)?;
                self.settle(head.promise, Settlement::Rejected(value))?;
                self.pop_async_request(id, head.promise)
            }
        }
    }
}
