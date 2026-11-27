//! Interpreter. A step either continues, leaves the frame, or throws.
//! Handlers run in one place; call sites do not each reimplement catch.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::builtin::is_callable_object;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::string::utf16_unit_string;
use crate::bytecode::{
    AccessorPrefix, BinaryOp, CallArgs, Constant, ExceptionHandler, Function, Instr,
    PropertyDefinition, Slot, SlotKind, UnaryOp,
};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{
    AsyncCompletion, AsyncRequest, Attributes, BoundObject, ClassElement, DescriptorUpdate,
    GeneratorStatus, HAS_INSTANCE_SYMBOL, ITERATOR_SYMBOL, NativeObject, Object, OwnProperty,
    OwnPropertyLookup, PlainObject, PrivateElement, Property, TO_PRIMITIVE_SYMBOL, TypedIndex,
    object_id, string_own_property,
};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot as IntrinsicSlot};
use crate::ops::{self, BinaryError, unary_op};
use crate::resolve::AwaitPrepared;
use crate::runtime::{
    Arguments, Continuation, ContinuationTarget, ExecOutcome, Invocation, Settlement,
};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
mod branch;
mod call;
mod class;
mod env;
mod iterator;
mod object;
mod value;

type PropertySite = (usize, usize);
/// A Number that can address an array element without building a property key.
fn array_index_number(value: &Value) -> Option<usize> {
    let ValueRef::Number(number) = value.view() else {
        return None;
    };
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number >= u32::MAX as f64 {
        return None;
    }
    Some(number as usize)
}

#[derive(Clone, Copy)]
enum CacheStatus {
    Empty,
    Seen(u8),
    Hit {
        symbol: bool,
        layout: u64,
        slot: u32,
        unstable: bool,
    },
    Mismatch {
        unstable: bool,
    },
    Disabled,
}

const FUNCTION_CALL: IntrinsicKey = IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "call");
const FUNCTION_APPLY: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "apply");
const FUNCTION_BIND: IntrinsicKey = IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "bind");
pub(crate) const FUNCTION_HAS_INSTANCE: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::PrototypeSymbol, "hasInstance");

pub(crate) const FUNCTION_CTOR: IntrinsicKey =
    IntrinsicKey::new("Function", IntrinsicSlot::Global, "");
const FUNCTION_PROTO: IntrinsicKey = IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "");
pub(crate) const THROW_TYPE_ERROR: IntrinsicKey =
    IntrinsicKey::new("ThrowTypeError", IntrinsicSlot::Prototype, "");
pub(crate) const FUNCTION_METHODS: [IntrinsicKey; 4] = [
    FUNCTION_CALL,
    FUNCTION_APPLY,
    FUNCTION_BIND,
    IntrinsicKey::new("Function", IntrinsicSlot::Prototype, "toString"),
];
pub(crate) struct FunctionIntrinsics<P>(std::marker::PhantomData<P>);

impl<P: PlatformServices> FunctionIntrinsics<P> {
    pub(crate) const INTRINSICS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::constructor(
            FUNCTION_CTOR,
            1,
            |vm, _, roots, _, _| {
                vm.throw_type_error(roots, "Dynamic function construction is disabled")
            },
            |vm, _, roots, _, _| {
                vm.throw_type_error(roots, "Dynamic function construction is disabled")
            },
        )
        .without_new_target_prototype(),
        IntrinsicDef::call(FUNCTION_PROTO, 0, |_, _, _, _, _| {
            Ok(Invocation::Value(Value::UNDEFINED))
        }),
        IntrinsicDef::call(THROW_TYPE_ERROR, 0, |vm, _, roots, _, _| {
            vm.throw_type_error(roots, "Restricted function property")
        }),
        IntrinsicDef::call(FUNCTION_CALL, 1, VmCore::function_call),
        IntrinsicDef::call(FUNCTION_APPLY, 2, VmCore::function_apply),
        IntrinsicDef::call(FUNCTION_BIND, 1, VmCore::function_bind),
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
        IntrinsicDef::call(FUNCTION_METHODS[3], 0, VmCore::function_to_string),
    ];
}

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

impl VmCore {
    fn function_call<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if !self.is_callable(receiver) {
            return self.throw_type_error(roots, "incompatible Function receiver");
        }
        let this_arg = args.first().cloned().unwrap_or(Value::UNDEFINED);
        let rest = args.get(1..).unwrap_or_default();
        self.invoke(host, roots, receiver, Arguments::Values(rest), &this_arg)
    }

    fn function_apply<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if !self.is_callable(receiver) {
            return self.throw_type_error(roots, "incompatible Function receiver");
        }
        let this_arg = args.first().cloned().unwrap_or(Value::UNDEFINED);
        let args = match args.get(1) {
            None => Vec::new(),
            Some(list) if matches!(list.view(), ValueRef::Undefined | ValueRef::Null) => Vec::new(),
            Some(list) => {
                let live = (receiver, &this_arg, list);
                match self.arguments_from_array_like(host, &roots.with(&live), list)? {
                    Ok(values) => values,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
        };
        self.invoke(host, roots, receiver, Arguments::Values(&args), &this_arg)
    }

    fn function_bind<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let target = match receiver.view() {
            ValueRef::Object(id) if self.heap.get(id).is_ok_and(is_callable_object) => id,
            _ => return self.throw_type_error(roots, "incompatible Function receiver"),
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

    fn function_to_string<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        _: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return self
                .throw_type_error(roots, "incompatible Function.prototype.toString receiver");
        };
        if !is_callable_object(self.heap.get(id)?) {
            return self
                .throw_type_error(roots, "incompatible Function.prototype.toString receiver");
        }
        let name = self.initial_function_name(host, id)?;
        let mut text = JsString::from("function ");
        text.push_str(&name);
        text.push_str(&"() { [native code] }".into());
        Ok(Invocation::Value(Value::string(text)))
    }

    fn initial_function_name<P: PlatformServices>(
        &self,
        host: &Host<P>,
        id: ObjectId,
    ) -> Result<JsString, VmError> {
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
                intrinsic.definition::<crate::Platform>().key.initial_name()
            }
            Object::Native(NativeObject::Host { target, .. }) => {
                crate::vm::host_function_metadata(host.bindings(), *target)
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
        let definition = &self.build.functions[self.function_index(func_id)?];
        let length = definition.length;
        let Object::Function(function) = self.heap.get(id)? else {
            return Err(VmError::InvalidState("expected script function".into()));
        };
        let name = function
            .dynamic_name
            .clone()
            .unwrap_or_else(|| definition.initial_name.as_str().into());
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

impl VmCore {
    pub(crate) fn execute_frame<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        mut frame: Frame,
    ) -> Result<ExecOutcome, VmError> {
        // Own a read-only build handle so dispatch can borrow the code across
        // mutable VM operations, including nested calls.
        let build = self.build.clone();
        let code = &build.functions[frame.function].code;
        loop {
            let pc = frame.pc;
            let Some(instr) = code.get(pc) else {
                return Ok(ExecOutcome::Return(Value::UNDEFINED));
            };
            frame.pc += 1;
            if crate::perf::ENABLED {
                crate::perf::record(|counters| counters.dispatches += 1);
            }
            match self.step(host, roots, &mut frame, instr, &build.constants, pc)? {
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

    pub(crate) fn resume_continuation<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn is_comparison(op: BinaryOp) -> bool {
        matches!(
            op,
            BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::StrictEq
                | BinaryOp::StrictNe
        )
    }

    /// Evaluates a comparison for a branch without materializing a boolean
    /// `Value` when both operands are numbers.
    fn branch_compare<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    ) -> Result<Result<bool, Value>, VmError> {
        if let (ValueRef::Number(left), ValueRef::Number(right)) = (lhs.view(), rhs.view())
            && let Some(result) = match op {
                BinaryOp::Lt => Some(left < right),
                BinaryOp::Le => Some(left <= right),
                BinaryOp::Gt => Some(left > right),
                BinaryOp::Ge => Some(left >= right),
                BinaryOp::Eq | BinaryOp::StrictEq => Some(left == right),
                BinaryOp::Ne | BinaryOp::StrictNe => Some(left != right),
                _ => None,
            }
        {
            return Ok(Ok(result));
        }
        match self.binary_value(host, roots, op, lhs, rhs)? {
            Ok(value) => Ok(Ok(value.truthy())),
            Err(error) => Ok(Err(error)),
        }
    }

    /// Evaluates a unary operation without committing a destination slot.
    fn unary_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        op: UnaryOp,
        mut value: Value,
    ) -> Result<Result<Value, Value>, VmError> {
        if let ValueRef::Number(number) = value.view()
            && let Some(result) = match op {
                UnaryOp::Neg => Some(Value::number(-number)),
                UnaryOp::Pos => Some(Value::number(number)),
                UnaryOp::BitNot => Some(Value::number(
                    (!(ops::number_to_uint32(number) as i32)) as f64,
                )),
                UnaryOp::ToNumeric => Some(Value::number(number)),
                UnaryOp::Increment => Some(Value::number(number + 1.0)),
                UnaryOp::Decrement => Some(Value::number(number - 1.0)),
                UnaryOp::Not | UnaryOp::TypeOf | UnaryOp::Void => None,
            }
        {
            return Ok(Ok(result));
        }
        if ops::unary_coerces(op) && matches!(value.view(), ValueRef::Object(_)) {
            match self.primitive_from_object(host, roots, &value, Hint::Number)? {
                Invocation::Value(primitive) => value = primitive,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        }
        if ops::unary_coerces(op) && matches!(value.view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(roots, ops::SYMBOL_TO_NUMBER)?));
        }
        match self.unary(op, value) {
            Ok(value) => Ok(Ok(value)),
            Err(BinaryError::TypeError(message)) => Ok(Err(self.type_error(roots, message)?)),
            Err(BinaryError::RangeError(message)) => {
                Ok(Err(self.make_range_error(roots, message)?))
            }
            Err(BinaryError::Vm(error)) => Err(error),
        }
    }

    fn binary_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        op: BinaryOp,
        mut lhs: Value,
        mut rhs: Value,
    ) -> Result<Result<Value, Value>, VmError> {
        if let (ValueRef::Number(left), ValueRef::Number(right)) = (lhs.view(), rhs.view()) {
            return Ok(Ok(match op {
                BinaryOp::Add => Value::number(left + right),
                BinaryOp::Sub => Value::number(left - right),
                BinaryOp::Mul => Value::number(left * right),
                BinaryOp::Div => Value::number(left / right),
                BinaryOp::Rem => Value::number(left % right),
                BinaryOp::Exp => Value::number(left.powf(right)),
                BinaryOp::Lt => Value::boolean(left < right),
                BinaryOp::Le => Value::boolean(left <= right),
                BinaryOp::Gt => Value::boolean(left > right),
                BinaryOp::Ge => Value::boolean(left >= right),
                BinaryOp::Eq | BinaryOp::StrictEq => Value::boolean(left == right),
                BinaryOp::Ne | BinaryOp::StrictNe => Value::boolean(left != right),
                BinaryOp::BitAnd => {
                    let left = ops::number_to_uint32(left) as i32;
                    let right = ops::number_to_uint32(right);
                    Value::number((left & right as i32) as f64)
                }
                BinaryOp::BitOr => {
                    let left = ops::number_to_uint32(left) as i32;
                    let right = ops::number_to_uint32(right);
                    Value::number((left | right as i32) as f64)
                }
                BinaryOp::BitXor => {
                    let left = ops::number_to_uint32(left) as i32;
                    let right = ops::number_to_uint32(right);
                    Value::number((left ^ right as i32) as f64)
                }
                BinaryOp::Shl => {
                    let left = ops::number_to_uint32(left) as i32;
                    let right = ops::number_to_uint32(right);
                    Value::number(left.wrapping_shl(right & 0x1f) as f64)
                }
                BinaryOp::Shr => {
                    let left = ops::number_to_uint32(left) as i32;
                    let right = ops::number_to_uint32(right);
                    Value::number(left.wrapping_shr(right & 0x1f) as f64)
                }
                BinaryOp::UShr => {
                    let left = ops::number_to_uint32(left);
                    let right = ops::number_to_uint32(right);
                    Value::number(left.wrapping_shr(right & 0x1f) as f64)
                }
            }));
        }
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
                    match self.primitive_from_object(host, &roots.with(other), value, hint)? {
                        Invocation::Value(primitive) => *value = primitive,
                        Invocation::Thrown(error) => return Ok(Err(error)),
                    }
                }
                if numeric && matches!(value.view(), ValueRef::Symbol(_)) {
                    return Ok(Err(self.type_error(roots, ops::SYMBOL_TO_NUMBER)?));
                }
            }
        }
        Ok(match ops::binary_op(op, lhs, rhs) {
            Ok(value) => Ok(value),
            Err(BinaryError::TypeError(message)) => Err(self.type_error(roots, message)?),
            Err(BinaryError::RangeError(message)) => Err(self.make_range_error(roots, message)?),
            Err(BinaryError::Vm(error)) => return Err(error),
        })
    }

    fn step<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        constants: &[Constant],
        pc: usize,
    ) -> Result<Step, VmError> {
        match *instr {
            Instr::Const { .. }
            | Instr::Undefined { .. }
            | Instr::Move { .. }
            | Instr::Binary { .. }
            | Instr::BinaryConst { .. }
            | Instr::Unary { .. }
            | Instr::ToString { .. }
            | Instr::ToPropertyKey { .. }
            | Instr::Return { .. }
            | Instr::Throw { .. }
            | Instr::ThrowDeleteSuper
            | Instr::ThrowAssignmentToConst => {
                self.step_value(host, roots, frame, instr, constants)
            }
            Instr::Jump { .. }
            | Instr::BranchFalse { .. }
            | Instr::BranchTrue { .. }
            | Instr::BranchCompareFalse { .. }
            | Instr::BranchCompareConstFalse { .. }
            | Instr::BranchCompareTrue { .. }
            | Instr::BranchCompareConstTrue { .. }
            | Instr::BranchBinary { .. }
            | Instr::BranchBinaryConst { .. }
            | Instr::BranchUnary { .. } => self.step_branch(host, roots, frame, instr, constants),
            Instr::GetImport { .. }
            | Instr::GetModuleBinding { .. }
            | Instr::GetModuleNamespace { .. }
            | Instr::ImportModule { .. }
            | Instr::GetBuiltin { .. }
            | Instr::GetTemplate { .. }
            | Instr::GetBinding { .. }
            | Instr::SetBinding { .. }
            | Instr::GetBindingDirect { .. }
            | Instr::SetBindingDirect { .. }
            | Instr::GetModuleBindingDirect { .. }
            | Instr::SetModuleBindingDirect { .. }
            | Instr::DeclareBinding { .. }
            | Instr::SaveEnv { .. }
            | Instr::NewIterationEnv { .. }
            | Instr::RestoreEnv { .. } => self.step_env(host, roots, frame, instr, constants),
            Instr::CallValue { .. }
            | Instr::Construct { .. }
            | Instr::SetFunctionName { .. }
            | Instr::SetHome { .. }
            | Instr::GetSuperBase { .. }
            | Instr::GetSuper { .. }
            | Instr::SetSuper { .. }
            | Instr::GetDerivedThis { .. }
            | Instr::GetSuperConstructor { .. }
            | Instr::SuperConstruct { .. }
            | Instr::Await { .. }
            | Instr::Yield { .. }
            | Instr::GeneratorStart
            | Instr::LoadGeneratorReturn { .. } => {
                self.step_call(host, roots, frame, instr, constants)
            }
            Instr::Closure { .. }
            | Instr::NewClassScope { .. }
            | Instr::LoadClassBinding { .. }
            | Instr::InitClassBinding { .. }
            | Instr::CreateClass { .. }
            | Instr::ScopedClosure { .. }
            | Instr::DefineClassMethod { .. }
            | Instr::AddClassField { .. }
            | Instr::AddClassBlock { .. }
            | Instr::RunClassStatic { .. }
            | Instr::AddClassPrivate { .. }
            | Instr::GetPrivate { .. }
            | Instr::SetPrivate { .. }
            | Instr::HasPrivate { .. } => self.step_class(host, roots, frame, instr, constants),
            Instr::NewObject { .. }
            | Instr::NewRegExp { .. }
            | Instr::NewArray { .. }
            | Instr::ArrayAppend { .. }
            | Instr::ArrayHole { .. }
            | Instr::GetMember { .. }
            | Instr::GetMemberConstKey { .. }
            | Instr::GetDestructuredMember { .. }
            | Instr::HasProperty { .. }
            | Instr::InstanceOf { .. }
            | Instr::DeleteMember { .. }
            | Instr::SetMember { .. }
            | Instr::DefineProperty { .. }
            | Instr::CopyDataProperties { .. }
            | Instr::AssertObjectCoercible { .. }
            | Instr::RequireObject { .. }
            | Instr::ObjectRest { .. } => {
                self.step_object(host, roots, frame, instr, constants, pc)
            }
            Instr::GetForInIterator { .. }
            | Instr::ForInNext { .. }
            | Instr::GetIterator { .. }
            | Instr::IteratorCall { .. }
            | Instr::IteratorNext { .. }
            | Instr::IteratorSkip { .. }
            | Instr::IteratorClose { .. } => {
                self.step_iterator(host, roots, frame, instr, constants)
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    fn call_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        dst: Slot,
        callee: Slot,
        args: &CallArgs,
        receiver: Option<Slot>,
    ) -> Result<Step, VmError> {
        let callee = frame.read(callee)?;
        let receiver = match receiver {
            Some(slot) => frame.read(slot)?,
            None => Value::UNDEFINED,
        };
        let invocation = match args {
            CallArgs::Slots(slots) => self.invoke(
                host,
                &roots.with(&*frame),
                &callee,
                Arguments::Slots(frame, slots),
                &receiver,
            )?,
            CallArgs::Spread(slot) => {
                let values = self.spread_arguments(frame, *slot)?;
                self.invoke(
                    host,
                    &roots.with(&*frame),
                    &callee,
                    Arguments::Values(&values),
                    &receiver,
                )?
            }
        };
        store_invocation(frame, dst, invocation)
    }

    fn spread_arguments(&self, frame: &Frame, slot: Slot) -> Result<Vec<Value>, VmError> {
        let message = "spread arguments require a dense data array";
        let id = object_id(frame.read_ref(slot)?, message)?;
        let Object::Array(array) = self.heap.get(id)? else {
            return Err(VmError::InvalidState(message.to_owned()));
        };
        array.dense_values()
    }

    pub(crate) fn get_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
            return Ok(Err(self.type_error(roots, "value is not iterable")?));
        };
        self.get_iterator_from_method(host, roots, iterable, &method)
    }

    pub(crate) fn get_iterator_from_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        iterable: &Value,
        method: &Value,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        let result = self.invoke(host, roots, method, Arguments::EMPTY, iterable)?;
        let iterator =
            self.require_iterator_object(roots, result, "Symbol.iterator must return an object")?;
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
    pub(crate) fn iterator_step<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    pub(crate) fn iterator_step_result<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        iterator: &Value,
        next: &Value,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let result = self.invoke(host, roots, next, Arguments::EMPTY, iterator)?;
        let result = match self.require_iterator_object(
            roots,
            result,
            "iterator next must return an object",
        )? {
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

    pub(crate) fn close_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        iterator: &Value,
    ) -> Result<Invocation, VmError> {
        // Valid bytecode produced by the compiler only supplies a GetIterator result.
        if !matches!(iterator.view(), ValueRef::Object(_)) {
            return self.throw_type_error(roots, "iterator must be an object");
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
        let invocation = self.invoke(host, roots, &method, Arguments::EMPTY, iterator)?;
        self.require_iterator_object(roots, invocation, "iterator return must return an object")
    }

    pub(crate) fn string_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Invocation, VmError> {
        Ok(match self.coerce_string(host, roots, value)? {
            Ok(string) => Invocation::Value(Value::string(string)),
            Err(error) => Invocation::Thrown(error),
        })
    }

    pub(crate) fn coerce_string<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::String)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(value.as_ref().view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(roots, ops::SYMBOL_TO_STRING)?));
        }
        Ok(Ok(ops::to_js_string(&value)))
    }

    /// `Number(value)` accepts BigInt by an explicit precision-losing conversion,
    /// unlike arithmetic and `Number.prototype` methods.
    pub(crate) fn coerce_number_allow_bigint<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
            return Ok(Err(self.type_error(roots, ops::SYMBOL_TO_NUMBER)?));
        }
        Ok(Ok(ops::to_number(&value)?))
    }

    pub(crate) fn coerce_number<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.coerce_primitive(host, roots, value, Hint::Number)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(value.as_ref().view(), ValueRef::Symbol(_)) {
            return Ok(Err(self.type_error(roots, ops::SYMBOL_TO_NUMBER)?));
        }
        if matches!(value.as_ref().view(), ValueRef::BigInt(_)) {
            return Ok(Err(self.type_error(roots, ops::BIGINT_TO_NUMBER)?));
        }
        Ok(Ok(ops::to_number(&value)?))
    }

    pub(crate) fn coerce_property_key<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    pub(crate) fn coerce_primitive<'a, P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn primitive_from_object<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
                    Arguments::Values(&[Value::string(hint.as_str().into())]),
                    value,
                )?;
                return match result {
                    Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {
                        self.throw_type_error(roots, "Cannot convert object to primitive value")
                    }
                    result => Ok(result),
                };
            }
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        }
        self.ordinary_to_primitive(host, roots, value, hint)
    }

    pub(crate) fn ordinary_to_primitive<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
            match self.invoke(host, roots, &method, Arguments::EMPTY, value)? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {}
                Invocation::Value(primitive) => return Ok(Invocation::Value(primitive)),
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        }
        self.throw_type_error(roots, "Cannot convert object to primitive value")
    }

    fn require_iterator_object(
        &mut self,
        roots: &ActiveRoots<'_>,
        invocation: Invocation,
        message: &str,
    ) -> Result<Invocation, VmError> {
        match &invocation {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => {
                Ok(invocation)
            }
            Invocation::Thrown(_) => Ok(invocation),
            Invocation::Value(_) => self.throw_type_error(roots, message),
        }
    }

    pub(crate) fn read_member<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(roots, "property access on a non-object");
        }
        if let ValueRef::Object(id) = object.view()
            && let Some(index) = array_index_number(key)
        {
            let property = match self.heap.get(id)? {
                Object::Array(array) => array.own_index(index).map(OwnProperty::into_owned),
                _ => None,
            };
            if let Some(property) = property {
                return self.read_property_value(host, roots, object, property);
            }
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

    fn read_destructured_member<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(roots, "Cannot destructure null or undefined");
        }
        self.read_property(host, roots, object, key)
    }

    pub(crate) fn read_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
            return self.throw_type_error(roots, "property access on a non-object");
        };
        self.read_property_with_receiver(host, roots, id, object, key)
    }

    pub(crate) fn read_property_with_receiver<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        receiver: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        self.read_property_from_chain(host, roots, target, key, |_| Cow::Borrowed(receiver))
    }

    fn cache_status(
        &self,
        site: PropertySite,
        receiver: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> CacheStatus {
        let Some(entry) = self
            .property_caches
            .get(site.0)
            .and_then(|caches| caches.get(site.1))
        else {
            return CacheStatus::Empty;
        };
        match entry {
            crate::vm::PropertyCache::Empty => CacheStatus::Empty,
            crate::vm::PropertyCache::Seen(count) => CacheStatus::Seen(*count),
            crate::vm::PropertyCache::Disabled => CacheStatus::Disabled,
            crate::vm::PropertyCache::Entry(entry) if entry.matches(receiver, key) => {
                CacheStatus::Hit {
                    symbol: entry.symbol(),
                    layout: entry.layout,
                    slot: entry.slot,
                    unstable: entry.unstable,
                }
            }
            crate::vm::PropertyCache::Entry(entry) => CacheStatus::Mismatch {
                unstable: entry.unstable,
            },
        }
    }

    fn mark_cache_stable(&mut self, site: PropertySite) {
        if let Some(caches) = self.property_caches.get_mut(site.0)
            && let Some(crate::vm::PropertyCache::Entry(entry)) = caches.get_mut(site.1)
        {
            entry.unstable = false;
        }
    }

    fn set_cache(&mut self, site: PropertySite, entry: crate::vm::PropertyCache) {
        if let Some(caches) = self.property_caches.get_mut(site.0) {
            if caches.len() <= site.1 {
                caches.resize_with(site.1 + 1, Default::default);
            }
            caches[site.1] = entry;
        }
    }

    /// Get on a ToObject result. Keep the same wrapper across accesses, but
    /// defer creating it until an accessor or proxy can observe the receiver.
    pub(crate) fn read_to_object_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn read_property_from_chain<'a, P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKeyRef<'_>,
        receiver: impl FnOnce(&mut Self) -> Cow<'a, Value>,
    ) -> Result<Invocation, VmError> {
        if crate::perf::ENABLED {
            crate::perf::record(|counters| counters.property_slow_paths += 1);
        }
        let mut current = Some(target);
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Proxy { slots, .. } = *object {
                let Some((target, handler)) = slots else {
                    return self.throw_type_error(roots, "Proxy has been revoked");
                };
                let receiver = receiver(self);
                return self.proxy_get(host, roots, target, handler, key, &receiver);
            }
            let property = match self.heap.lookup_own_property(object, key)? {
                OwnPropertyLookup::Found(property) => property,
                OwnPropertyLookup::Missing => {
                    current = object.prototype();
                    continue;
                }
                OwnPropertyLookup::InvalidTypedIndex => {
                    return Ok(Invocation::Value(Value::UNDEFINED));
                }
            };
            return match property.into_owned() {
                Property::Data(value) => Ok(Invocation::Value(value)),
                Property::Accessor { get: Some(get), .. } => {
                    let receiver = receiver(self);
                    self.invoke(
                        host,
                        roots,
                        &Value::object(get),
                        Arguments::EMPTY,
                        &receiver,
                    )
                }
                Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::UNDEFINED)),
            };
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    pub(crate) fn read_array_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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

    fn read_property_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        property: Property,
    ) -> Result<Invocation, VmError> {
        match property {
            Property::Data(value) => Ok(Invocation::Value(value)),
            Property::Accessor { get: Some(get), .. } => {
                self.invoke(host, roots, &Value::object(get), Arguments::EMPTY, receiver)
            }
            Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::UNDEFINED)),
        }
    }

    fn instance_of<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        constructor: &Value,
    ) -> Result<Invocation, VmError> {
        self.has_instance(host, roots, value, constructor, false)
    }

    pub(crate) fn ordinary_has_instance<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        constructor: &Value,
    ) -> Result<Invocation, VmError> {
        self.has_instance(host, roots, value, constructor, true)
    }

    fn has_instance<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
                self.throw_type_error(roots, "right-hand side of 'instanceof' is not an object")
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
                        return self.throw_type_error(roots, "Symbol.hasInstance is not callable");
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
                            match self.invoke(
                                host,
                                roots,
                                &hook,
                                Arguments::Values(std::slice::from_ref(value)),
                                &receiver,
                            )? {
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
                    self.throw_type_error(roots, not_callable)
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
                    return self.throw_type_error(
                        roots,
                        "'instanceof' constructor prototype is not an object",
                    );
                };
                id
            }
            Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
        };
        // Prototype links are kept acyclic by set_prototype and snapshot restore.
        loop {
            let parent = match self.proxy_prototype(host, roots, current)? {
                Ok(Some(parent)) => parent,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if parent == prototype {
                return Ok(Invocation::Value(Value::boolean(true)));
            }
            current = parent;
        }
        Ok(Invocation::Value(Value::boolean(false)))
    }

    fn has_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        let ValueRef::Object(id) = object.view() else {
            return self.throw_type_error(roots, "right-hand side of 'in' is not an object");
        };
        let roots = &roots.with(object);
        let key = match self.coerce_property_key(host, roots, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.has_property_key(host, roots, id, key.borrowed())
    }

    pub(crate) fn has_property_key<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        let mut current = Some(target);
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Proxy { slots, .. } = *object {
                let Some((target, handler)) = slots else {
                    return self.throw_type_error(roots, "Proxy has been revoked");
                };
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

    pub(crate) fn delete_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(roots, "cannot delete a property of null or undefined");
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
            && let Object::Proxy { slots, .. } = *self.heap.get(id)?
        {
            let Some((target, handler)) = slots else {
                return self.throw_type_error(roots, "Proxy has been revoked");
            };
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

    fn write_member<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        object: &Value,
        key: &Value,
        value: Value,
        site: Option<PropertySite>,
    ) -> Result<Invocation, VmError> {
        if matches!(object.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(roots, "property write on a non-object");
        }
        if let ValueRef::Object(id) = object.view()
            && let Some(index) = array_index_number(key)
        {
            let written = match self.heap.get_mut(id)? {
                Object::Array(array) => array.set_existing_index(index, &value),
                _ => false,
            };
            if written {
                return Ok(Invocation::Value(Value::boolean(true)));
            }
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
                    return self
                        .throw_type_error(roots, "Cannot assign to a read only string property");
                }
                self.primitive_prototype(object)
                    .expect("non-nullish primitive")
            }
        };
        let mut populate = false;
        let mut unstable = false;
        if let Some(site) = site
            && let ValueRef::Object(receiver) = object.view()
            && receiver == id
        {
            match self.cache_status(site, id, key.borrowed()) {
                CacheStatus::Hit {
                    symbol,
                    layout,
                    slot,
                    unstable: was_unstable,
                } => {
                    if self
                        .heap
                        .set_plain_slot(id, symbol, layout, slot, value.clone())?
                    {
                        if was_unstable {
                            self.mark_cache_stable(site);
                        }
                        return Ok(Invocation::Value(Value::boolean(true)));
                    }
                    self.set_cache(site, crate::vm::PropertyCache::Empty);
                    populate = true;
                }
                CacheStatus::Mismatch {
                    unstable: was_unstable,
                } => {
                    if was_unstable {
                        self.set_cache(site, crate::vm::PropertyCache::Disabled);
                    } else {
                        unstable = true;
                        populate = true;
                    }
                }
                CacheStatus::Disabled => {}
                CacheStatus::Empty => {
                    self.set_cache(site, crate::vm::PropertyCache::Seen(1));
                }
                CacheStatus::Seen(count) => {
                    if count >= 32 {
                        populate = true;
                    } else {
                        self.set_cache(site, crate::vm::PropertyCache::Seen(count + 1));
                    }
                }
            }
        }
        let cache_key = populate.then(|| key.clone());
        let result = self.set_property(host, roots, id, object, key, value, true)?;
        if populate
            && let Some(site) = site
            && let Invocation::Value(returned) = &result
            && matches!(returned.view(), ValueRef::Bool(true))
            && let ValueRef::Object(receiver) = object.view()
            && receiver == id
            && let Some(cache_key) = cache_key
        {
            if let Some((layout, slot)) =
                self.heap.plain_data_slot(id, cache_key.borrowed(), true)?
            {
                self.set_cache(
                    site,
                    crate::vm::PropertyCache::Entry(crate::vm::PropertyCacheEntry {
                        receiver: id,
                        key: cache_key,
                        layout,
                        slot,
                        unstable,
                    }),
                );
            } else {
                self.set_cache(site, crate::vm::PropertyCache::Disabled);
            }
        }
        Ok(result)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    pub(crate) fn set_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
                vm.throw_type_error(roots, message)
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
            if let Object::Proxy { slots, .. } = *object {
                let Some((target, handler)) = slots else {
                    return self.throw_type_error(roots, "Proxy has been revoked");
                };
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
                return match self.invoke(
                    host,
                    roots,
                    &Value::object(set),
                    Arguments::Values(&[value]),
                    receiver,
                )? {
                    Invocation::Value(_) => Ok(success),
                    thrown @ Invocation::Thrown(_) => Ok(thrown),
                };
            }
            Some(None) => return reject(self, "Cannot set property without a setter"),
            None if property.is_some_and(|property| !property.attributes().writable) => {
                if strict && let PropertyKey::String(key) = &key {
                    return self.throw_type_error(
                        roots,
                        &format!("Cannot assign to read only property '{key}'"),
                    );
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

    fn copy_data_properties<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
    pub(crate) fn initialize_class_elements<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
                self.type_error(roots, "private member is already installed")?,
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
                let arguments: Vec<Value> = key
                    .iter()
                    .filter(|_| needs_key)
                    .map(|key| match key {
                        PropertyKey::String(key) => Value::string(key.clone()),
                        PropertyKey::Symbol(key) => Value::symbol(*key),
                    })
                    .collect();
                match self.invoke(
                    host,
                    &roots.with(&key),
                    &Value::object(closure),
                    Arguments::Values(&arguments),
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
                    return Ok(Some(
                        self.type_error(roots, "private field is already installed")?,
                    ));
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

    fn suspend<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
        if crate::perf::ENABLED {
            crate::perf::capture(resume_entry.saved_slots.len());
        }
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
        if crate::perf::ENABLED {
            crate::perf::capture(record.values.len());
        }
        Ok(Step::Yield { value, delegate })
    }

    /// Build the generator object the call returns, without running the body.
    ///
    /// The instance prototype is the function's `.prototype` when that is an
    /// object and the shared generator prototype otherwise.
    fn start_generator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
        if crate::perf::ENABLED {
            crate::perf::capture(start.saved_slots.len());
        }
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

    fn constant(constants: &[Constant], index: u32) -> Result<Value, VmError> {
        let constant = constants
            .get(index as usize)
            .ok_or_else(|| VmError::InvalidState(format!("constant {index} does not exist")))?;
        Ok(match constant {
            Constant::Null => Value::NULL,
            Constant::Bool(value) => Value::boolean(*value),
            Constant::Number(value) => Value::number(*value),
            Constant::String(value) => Value::string(value.clone()),
            Constant::BigInt(value) => Value::bigint(value.clone()),
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

impl VmCore {
    /// Queue one request for an async generator method call.
    ///
    /// A receiver that is not an async generator still returns a Promise: the
    /// method rejects with a `TypeError` instead of throwing synchronously.
    pub(crate) fn resume_async_generator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Object(id) => id,
            _ => return self.reject_type_error(roots, "receiver is not an async generator"),
        };
        let Some(record) = self.heap.generator(id) else {
            return self.reject_type_error(roots, "receiver is not an async generator");
        };
        let func_id = record.func_id;
        if !self.is_async_generator(func_id)? {
            return self.reject_type_error(roots, "receiver is not an async generator");
        }
        self.enqueue_async_request(host, roots, id, completion, argument)
    }

    /// A fresh Promise already rejected with a script `TypeError`.
    fn reject_type_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        let error = self.type_error(roots, message)?;
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
    pub(crate) fn resume_generator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        completion: GeneratorCompletion,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Object(id) => id,
            _ => return self.throw_type_error(roots, "generator method called on a non-object"),
        };
        let Some(record) = self.heap.generator(id).cloned() else {
            return self.throw_type_error(roots, "receiver is not a generator");
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
            return self.throw_type_error(roots, "generator is already executing");
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
    fn enqueue_async_request<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
    fn drive_async_generator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<(), VmError> {
        while self.async_generator_step(host, roots, id)? {}
        Ok(())
    }

    fn async_generator_step<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
    fn settle_finished_request<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
    pub(crate) fn finish_closing_request<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
