//! Interpreter. A step either continues, leaves the frame, or throws.
//! Handlers run in one place; call sites do not each reimplement catch.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::builtins::is_callable_object;
use crate::bytecode::{
    BinaryOp, CallArgs, ExceptionHandler, Function, Instr, PropertyDefinition, Slot, SlotKind,
    UnaryOp,
};
use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{
    DescriptorUpdate, ITERATOR_SYMBOL, Object, OwnProperty, Property, TO_PRIMITIVE_SYMBOL,
    object_id,
};
use crate::host::Host;
use crate::ops::{self, BinaryError, unary_op};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Continuation, ExecOutcome, Invocation, Settlement};
use crate::string::utf16_unit_string;
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

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
            match self.step(host, &mut frame, instr)? {
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
        continuation_id: crate::value::ContinuationId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let continuation = self.continuations.remove(&continuation_id).ok_or_else(|| {
            VmError::InvalidState(format!("continuation {continuation_id} does not exist"))
        })?;
        let function_index = self.function_index(continuation.func_id)?;
        let resume_entry = self.build.functions[function_index]
            .resumes
            .get(continuation.resume_id as usize)
            .cloned()
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
            .zip(continuation.values)
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
                frame.write(resume_entry.fulfill_dst, value)?;
                frame.pc = resume_entry.fulfill_pc as usize;
            }
            Settlement::Rejected(value) => {
                frame.write(resume_entry.reject_dst, value)?;
                frame.pc = resume_entry.reject_pc as usize;
            }
        }
        let outcome = self.execute_frame(host, frame)?;
        self.settle_outcome(host, async_promise, outcome)
    }

    fn step(
        &mut self,
        host: &mut dyn Host,
        frame: &mut Frame,
        instr: Instr,
    ) -> Result<Step, VmError> {
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
                    for value in [&mut lhs, &mut rhs] {
                        if matches!(value, Value::Object(_)) {
                            match self.primitive_from_object(host, value, hint)? {
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
                    match self.primitive_from_object(host, &value, Hint::Number)? {
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
                let invocation = self.string_value(host, frame.read_ref(src)?)?;
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
                let value = match self.coerce_property_key(host, frame.read_ref(src)?)? {
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
            } => self.call_value(host, frame, dst, callee, args, receiver),
            Instr::Construct { dst, callee, args } => {
                let args = self.read_call_args(frame, &args)?;
                let callee = frame.read(callee)?;
                let invocation = self.construct_with(host, &callee, args, &callee)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::Closure { dst, function } => self.make_closure(frame, dst, function),
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
                let invocation =
                    self.read_member(host, frame.read_ref(object)?, frame.read_ref(key)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::GetDestructuredMember { dst, object, key } => {
                let key = converted_property_key(frame.read_ref(key)?)?;
                let invocation =
                    self.read_destructured_member(host, frame.read_ref(object)?, key)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::HasProperty { dst, object, key } => {
                let invocation =
                    self.has_property(host, frame.read_ref(object)?, frame.read_ref(key)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::InstanceOf {
                dst,
                value,
                constructor,
            } => {
                let invocation =
                    self.instance_of(host, frame.read_ref(value)?, frame.read_ref(constructor)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::DeleteMember { dst, object, key } => {
                let invocation =
                    self.delete_property(host, frame.read_ref(object)?, frame.read_ref(key)?)?;
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
                let key = match self.for_in_next(host, iterator)? {
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
                    match self.get_iterator(host, frame.read_ref(iterable)?)? {
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
                let invocation = self.close_iterator(host, &frame.read(iterator)?)?;
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
                let key = match self.coerce_property_key(host, frame.read_ref(key)?)? {
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
                match self.copy_data_properties(host, target, source, &excluded)? {
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
            Instr::Await { input, resume, .. } => self.suspend(host, frame, input, resume),
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
            Instr::ThrowAssignmentToConst => Ok(Step::Throw(
                self.type_error("Assignment to constant variable.")?,
            )),
        }
    }

    fn call_value(
        &mut self,
        host: &mut dyn Host,
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
        let invocation = self.invoke(host, &callee, args, &receiver)?;
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
        iterable: &Value,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        let method = if matches!(iterable, Value::Object(_) | Value::String(_)) {
            match self.read_property(host, iterable, PropertyKeyRef::Symbol(ITERATOR_SYMBOL))? {
                Invocation::Value(method) => method,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            return Ok(Err(self.type_error("value is not iterable")?));
        };
        self.get_iterator_from_method(host, iterable, &method)
    }

    pub(crate) fn get_iterator_from_method(
        &mut self,
        host: &mut dyn Host,
        iterable: &Value,
        method: &Value,
    ) -> Result<Result<(Value, Value), Value>, VmError> {
        let result = self.invoke(host, method, Vec::new(), iterable)?;
        let iterator =
            self.require_iterator_object(result, "Symbol.iterator must return an object")?;
        let iterator = match iterator {
            Invocation::Value(iterator) => iterator,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        match self.read_property(host, &iterator, PropertyKeyRef::String("next"))? {
            Invocation::Value(next) => Ok(Ok((iterator, next))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    /// A failed step is not a consumer-side abrupt exit and does not close the iterator.
    pub(crate) fn iterator_step(
        &mut self,
        host: &mut dyn Host,
        iterator: &Value,
        next: &Value,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let result = match self.iterator_step_result(host, iterator, next)? {
            Ok(Some(result)) => result,
            other => return Ok(other),
        };
        match self.read_property(host, &result, PropertyKeyRef::String("value"))? {
            Invocation::Value(value) => Ok(Ok(Some(value))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn iterator_step_result(
        &mut self,
        host: &mut dyn Host,
        iterator: &Value,
        next: &Value,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let result = self.invoke(host, next, Vec::new(), iterator)?;
        let result =
            match self.require_iterator_object(result, "iterator next must return an object")? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        match self.read_property(host, &result, PropertyKeyRef::String("done"))? {
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
        iterator: &Value,
    ) -> Result<Invocation, VmError> {
        // Valid bytecode produced by the compiler only supplies a GetIterator result.
        if !matches!(iterator, Value::Object(_)) {
            return self.throw_type_error("iterator must be an object");
        }
        let method = match self.read_property(host, iterator, PropertyKeyRef::String("return"))? {
            Invocation::Value(Value::Undefined | Value::Null) => {
                return Ok(Invocation::Value(Value::Undefined));
            }
            Invocation::Value(method) => method,
            thrown => return Ok(thrown),
        };
        let invocation = self.invoke(host, &method, Vec::new(), iterator)?;
        self.require_iterator_object(invocation, "iterator return must return an object")
    }

    pub(crate) fn string_value(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
    ) -> Result<Invocation, VmError> {
        Ok(match self.coerce_string(host, value)? {
            Ok(string) => Invocation::Value(Value::String(string)),
            Err(error) => Invocation::Thrown(error),
        })
    }

    pub(crate) fn coerce_string(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let value = match self.coerce_primitive(host, value, Hint::String)? {
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
        value: &Value,
    ) -> Result<Result<f64, Value>, VmError> {
        let value = match self.coerce_primitive(host, value, Hint::Number)? {
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
        value: &Value,
    ) -> Result<Result<PropertyKey, Value>, VmError> {
        let value = match self.coerce_primitive(host, value, Hint::String)? {
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
        value: &'a Value,
        hint: Hint,
    ) -> Result<Result<Cow<'a, Value>, Value>, VmError> {
        if !matches!(value, Value::Object(_)) {
            return Ok(Ok(Cow::Borrowed(value)));
        }
        Ok(match self.primitive_from_object(host, value, hint)? {
            Invocation::Value(value) => Ok(Cow::Owned(value)),
            Invocation::Thrown(error) => Err(error),
        })
    }

    fn primitive_from_object(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        hint: Hint,
    ) -> Result<Invocation, VmError> {
        match self.read_property(host, value, PropertyKeyRef::Symbol(TO_PRIMITIVE_SYMBOL))? {
            Invocation::Value(Value::Undefined | Value::Null) => {}
            Invocation::Value(method) => {
                let result = self.invoke(
                    host,
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
            let method = match self.read_property(host, value, PropertyKeyRef::String(name))? {
                Invocation::Value(method) => method,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            if !self.is_callable(&method) {
                continue;
            }
            match self.invoke(host, &method, Vec::new(), value)? {
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
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if !matches!(
            object,
            Value::Object(_) | Value::String(_) | Value::Number(_)
        ) {
            return self.throw_type_error("property access on a non-object");
        }
        match key {
            Value::String(key) => {
                return self.read_property(host, object, PropertyKeyRef::JsString(key));
            }
            Value::Symbol(id) => {
                return self.read_property(host, object, PropertyKeyRef::Symbol(*id));
            }
            _ => {}
        }
        let key = match self.coerce_property_key(host, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.read_property(host, object, key.borrowed())
    }

    fn read_destructured_member(
        &mut self,
        host: &mut dyn Host,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        match object {
            Value::Object(_) | Value::Number(_) => return self.read_property(host, object, key),
            Value::Null | Value::Undefined => {
                return self.throw_type_error("Cannot destructure null or undefined");
            }
            Value::String(text) => match key {
                PropertyKeyRef::Symbol(ITERATOR_SYMBOL) => {
                    return self.read_property(host, object, key);
                }
                _ if key.is_str("length") => {
                    return Ok(Invocation::Value(Value::Number(text.unit_len() as f64)));
                }
                _ => {
                    if let Some(unit) =
                        ops::canonical_index_key(key).and_then(|index| text.unit_at(index))
                    {
                        return Ok(Invocation::Value(Value::String(utf16_unit_string(unit))));
                    }
                }
            },
            _ => {}
        }
        self.read_property_with_receiver(host, self.builtins.object_prototype, object, key)
    }

    pub(crate) fn read_property(
        &mut self,
        host: &mut dyn Host,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        // Primitive strings resolve indexed reads without allocating wrappers.
        if let Value::String(text) = object {
            if matches!(key, PropertyKeyRef::Symbol(_)) {
                return self.read_property_with_receiver(
                    host,
                    self.builtins.string_prototype,
                    object,
                    key,
                );
            }
            if key.is_str("length") {
                return Ok(Invocation::Value(Value::Number(text.unit_len() as f64)));
            }
            if let Some(index) = ops::canonical_index_key(key) {
                let value = text.unit_at(index).map_or(Value::Undefined, |unit| {
                    Value::String(utf16_unit_string(unit))
                });
                return Ok(Invocation::Value(value));
            }
            return self.read_property_with_receiver(
                host,
                self.builtins.string_prototype,
                object,
                key,
            );
        }
        if let Some(prototype) = self.primitive_prototype(object) {
            return self.read_property_with_receiver(host, prototype, object, key);
        }
        let Value::Object(id) = *object else {
            return self.throw_type_error("property access on a non-object");
        };
        self.read_property_with_receiver(host, id, object, key)
    }

    pub(crate) fn read_property_with_receiver(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        receiver: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        let mut current = Some(target);
        while let Some(id) = current {
            let object = self.heap.get(id)?;
            if let Object::Proxy {
                target, handler, ..
            } = *object
            {
                return self.proxy_get(host, target, handler, key, receiver);
            }
            if self.has_own_function_metadata(id, key)? {
                let name = if key.is_str("name") { "name" } else { "length" };
                return Ok(Invocation::Value(self.function_metadata(id, name)?));
            }
            if let Some(property) = object.own_property(key) {
                return self.read_property_value(host, receiver, property.into_owned());
            }
            current = object.prototype();
        }
        if let Some(name) = self.virtual_function_method(target, key) {
            let method = self.heap.alloc_builtin("Function", Some(name));
            return Ok(Invocation::Value(Value::Object(method)));
        }
        Ok(Invocation::Value(Value::Undefined))
    }

    pub(crate) fn read_array_index(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        index: usize,
    ) -> Result<Invocation, VmError> {
        let Object::Array(array) = self.heap.get(id)? else {
            unreachable!("array index reader requires an array")
        };
        let receiver = Value::Object(id);
        if let Some(property) = array.own_index(index).map(OwnProperty::into_owned) {
            self.read_property_value(host, &receiver, property)
        } else {
            self.read_property(host, &receiver, PropertyKeyRef::String(&index.to_string()))
        }
    }

    fn read_property_value(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        property: Property,
    ) -> Result<Invocation, VmError> {
        match property {
            Property::Data(value) => Ok(Invocation::Value(value)),
            Property::Accessor { get: Some(get), .. } => {
                self.invoke(host, &Value::Object(get), Vec::new(), receiver)
            }
            Property::Accessor { get: None, .. } => Ok(Invocation::Value(Value::Undefined)),
        }
    }

    fn instance_of(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        constructor: &Value,
    ) -> Result<Invocation, VmError> {
        let not_callable = "right-hand side of 'instanceof' is not callable";
        let Value::Object(mut target) = *constructor else {
            return self.throw_type_error(not_callable);
        };
        let mut remaining = self.heap.object_count();
        loop {
            let object = self.heap.get(target)?;
            if !is_callable_object(object) {
                return self.throw_type_error(not_callable);
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
        }

        let Value::Object(mut current) = *value else {
            return Ok(Invocation::Value(Value::Bool(false)));
        };
        let prototype = match self.read_property(
            host,
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
        object: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = *object else {
            return self.throw_type_error("right-hand side of 'in' is not an object");
        };
        let key = match self.coerce_property_key(host, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.has_property_key(host, id, key.borrowed())
    }

    pub(crate) fn has_property_key(
        &mut self,
        host: &mut dyn Host,
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
                return self.proxy_presence(host, target, handler, key, false);
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
        target: &Value,
        key: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(target, Value::Null | Value::Undefined) {
            return self.throw_type_error("cannot delete a property of null or undefined");
        }
        let coerced;
        let key = match key {
            Value::String(key) => PropertyKeyRef::JsString(key),
            Value::Symbol(id) => PropertyKeyRef::Symbol(*id),
            _ => {
                coerced = match self.coerce_property_key(host, key)? {
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
            return self.proxy_presence(host, target, handler, key, true);
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
            && (id == self.builtins.function_prototype || is_callable_object(self.heap.get(id)?)))
    }

    pub(crate) fn function_metadata(&self, id: ObjectId, key: &str) -> Result<Value, VmError> {
        let (object, bound_args, prefix) = match self.heap.get(id)? {
            Object::Bound(bound) => (self.heap.get(bound.target)?, bound.args.len(), "bound "),
            object => (object, 0, ""),
        };
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
            Object::BuiltinStatic(function) => {
                let member = function.member.as_deref();
                let length = match (function.name.as_str(), member) {
                    ("RegExp", None) => 2,
                    ("RegExp", Some("exec" | "test" | "escape")) => 1,
                    _ => 0,
                };
                (member.unwrap_or(&function.name), length)
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

    fn virtual_function_method(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Option<&'static str> {
        if self.heap.get(id).is_ok_and(is_callable_object) {
            ["call", "apply", "bind"]
                .into_iter()
                .find(|name| key.is_str(name))
        } else {
            None
        }
    }

    fn write_member(
        &mut self,
        host: &mut dyn Host,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = *object else {
            return self.throw_type_error("property write on a non-object");
        };
        let key = match self.coerce_property_key(host, key)? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        self.set_property(host, id, object, key, value, true)
    }

    pub(crate) fn set_property(
        &mut self,
        host: &mut dyn Host,
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
                return self.proxy_set(host, (target, handler), receiver, key, value, strict);
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
                return match self.invoke(host, &Value::Object(set), vec![value], receiver)? {
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
            let exists = match self.own_descriptor(host, id, key.borrowed())? {
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
            return match self.try_apply_property_update(host, id, key, update)? {
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
                return match self.try_apply_property_update(host, id, key, update)? {
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
        target: ObjectId,
        source: &Value,
        excluded: &BTreeSet<PropertyKeyRef<'_>>,
    ) -> Result<Invocation, VmError> {
        match source {
            Value::Object(id) => {
                // Capture the order once; an earlier getter may delete a later key
                // or change its enumerability before it is visited.
                let keys = match self.own_keys(host, *id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                for key in keys {
                    if excluded.contains(&key.borrowed()) {
                        continue;
                    }
                    match self.own_descriptor(host, *id, key.borrowed())? {
                        Ok(Some((_, attrs))) if attrs.enumerable => {}
                        Ok(_) => continue,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                    match self.read_property(host, source, key.borrowed())? {
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
        let prepared = match self.prepare_await(host, input_value)? {
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
