//! Interpreter. A step either continues, leaves the frame, or throws.
//! Handlers run in one place; call sites do not each reimplement catch.

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
use crate::host::{Host, HostReply};
use crate::ops::{self, BinaryError, unary_op};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Continuation, ExecOutcome, Invocation, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
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
        self.settle_outcome(async_promise, outcome)
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
            Instr::CallImport { dst, import, args } => {
                let args = self.read_call_args(frame, &args)?;
                frame.write(dst, self.call_import(host, import, &args)?)?;
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
                let invocation = self.construct(host, &callee, args)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::Closure { dst, function } => self.make_closure(frame, dst, function),
            Instr::NewObject { dst } => {
                let id = self.heap.alloc_plain();
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
            Instr::GetMember { dst, object, key } => {
                let invocation = self.read_member(host, &frame.read(object)?, &frame.read(key)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::HasProperty { dst, object, key } => {
                let invocation =
                    self.has_property(host, frame.read_ref(object)?, frame.read_ref(key)?)?;
                store_invocation(frame, dst, invocation)
            }
            Instr::GetIterator {
                iterator,
                next,
                iterable,
            } => {
                let value = match self.get_iterator(host, &frame.read(iterable)?)? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                };
                let next_method =
                    match self.read_property(host, &value, PropertyKeyRef::String("next"))? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => return Ok(Step::Throw(error)),
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
                let result =
                    self.invoke(host, &frame.read(next)?, Vec::new(), &frame.read(iterator)?)?;
                let result = match self
                    .require_iterator_object(result, "iterator next must return an object")?
                {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                };
                let is_done =
                    match self.read_property(host, &result, PropertyKeyRef::String("done"))? {
                        Invocation::Value(value) => value.truthy(),
                        Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                    };
                let item = if is_done {
                    Value::Undefined
                } else {
                    match self.read_property(host, &result, PropertyKeyRef::String("value"))? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                    }
                };
                frame.write(done, Value::Bool(is_done))?;
                frame.write(value, item)?;
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
                let id = self.builtin_id(builtin)?;
                frame.write(dst, Value::Object(id))?;
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
            Instr::Await { input, resume, .. } => {
                self.suspend(host, frame, input, resume)?;
                Ok(Step::Suspend)
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

    fn get_iterator(
        &mut self,
        host: &mut dyn Host,
        iterable: &Value,
    ) -> Result<Invocation, VmError> {
        if !matches!(iterable, Value::Object(_)) {
            return self.throw_type_error("value is not iterable");
        }
        let method =
            match self.read_property(host, iterable, PropertyKeyRef::Symbol(ITERATOR_SYMBOL))? {
                Invocation::Value(method) => method,
                thrown => return Ok(thrown),
            };
        let invocation = self.invoke(host, &method, Vec::new(), iterable)?;
        self.require_iterator_object(invocation, "Symbol.iterator must return an object")
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

    fn close_iterator(
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
        let primitive;
        let value = if matches!(value, Value::Object(_)) {
            primitive = match self.primitive_from_object(host, value, Hint::String)? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            &primitive
        } else {
            value
        };
        if matches!(value, Value::Symbol(_)) {
            return self.throw_type_error(ops::SYMBOL_TO_STRING);
        }
        Ok(Invocation::Value(Value::String(ops::to_js_string(value))))
    }

    pub(crate) fn coerce_property_key(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
    ) -> Result<Result<PropertyKey, Value>, VmError> {
        let primitive;
        let value = if matches!(value, Value::Object(_)) {
            primitive = match self.primitive_from_object(host, value, Hint::String)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            &primitive
        } else {
            value
        };
        Ok(Ok(match value {
            Value::Symbol(id) => PropertyKey::Symbol(*id),
            _ => PropertyKey::String(ops::to_js_string(value)),
        }))
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
                    vec![Value::String(hint.as_str().to_owned())],
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
        if !matches!(object, Value::Object(_)) {
            return self.throw_type_error("property access on a non-object");
        }
        match key {
            Value::String(key) => {
                return self.read_property(host, object, PropertyKeyRef::String(key));
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

    pub(crate) fn read_property(
        &mut self,
        host: &mut dyn Host,
        object: &Value,
        key: PropertyKeyRef<'_>,
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = *object else {
            return self.throw_type_error("property access on a non-object");
        };
        if let Some(property) = self
            .heap
            .find_property(id, key)?
            .map(OwnProperty::into_owned)
        {
            return self.read_property_value(host, object, property);
        }
        match key {
            PropertyKeyRef::String(key) if self.has_virtual_function_method(id, key) => {
                let method = self.heap.alloc_builtin("Function", Some(key));
                Ok(Invocation::Value(Value::Object(method)))
            }
            _ => Ok(Invocation::Value(Value::Undefined)),
        }
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
        let key = key.borrowed();
        let present = self.heap.has_property(id, key)?
            || matches!(key, PropertyKeyRef::String(key) if self.has_virtual_function_method(id, key));
        Ok(Invocation::Value(Value::Bool(present)))
    }

    fn has_virtual_function_method(&self, id: ObjectId, key: &str) -> bool {
        matches!(key, "call" | "apply" | "bind") && self.heap.function(id).is_some()
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
        let property = self.heap.find_property(id, key.borrowed())?;
        match property.as_ref().and_then(OwnProperty::setter) {
            Some(Some(set)) => {
                if let Invocation::Thrown(error) =
                    self.invoke(host, &Value::Object(set), vec![value], object)?
                {
                    return Ok(Invocation::Thrown(error));
                }
            }
            Some(None) => return self.throw_type_error("Cannot set property without a setter"),
            None if property.is_some_and(|property| !property.attributes().writable) => {
                let message = match &key {
                    PropertyKey::String(key) => {
                        format!("Cannot assign to read only property '{key}'")
                    }
                    PropertyKey::Symbol(_) => {
                        "Cannot assign to read only symbol property".to_owned()
                    }
                };
                return self.throw_type_error(&message);
            }
            None => match self.heap.get(id)? {
                Object::Array(_) if key.as_string() == Some("length") => {
                    return self.apply_property_update(
                        id,
                        key,
                        DescriptorUpdate {
                            value: Some(value),
                            ..DescriptorUpdate::default()
                        },
                    );
                }
                Object::Array(array)
                    if !array.length_writable
                        && key
                            .as_string()
                            .and_then(ops::array_index)
                            .is_some_and(|index| index >= array.length) =>
                {
                    return self.throw_type_error("Cannot extend array with read only length");
                }
                Object::Native(_) => {
                    return self.throw_type_error("native handles do not expose script properties");
                }
                _ => self.heap.set_own(id, key, value)?,
            },
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

    fn call_import(
        &mut self,
        host: &mut dyn Host,
        import: u32,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let import_name = self
            .build
            .imports
            .get(import as usize)
            .ok_or_else(|| VmError::InvalidState(format!("import {import} does not exist")))?
            .name
            .clone();
        let host_args = args
            .iter()
            .map(|arg| self.to_host_value(arg))
            .collect::<Result<Vec<_>, _>>()?;
        match host.call(&import_name, &host_args).map_err(VmError::Host)? {
            HostReply::Value(value) => self.host_to_value(&value),
            HostReply::Operation { operation_id } => {
                self.track_operation(import_name, operation_id)
            }
        }
    }

    fn track_operation(&mut self, import: String, operation_id: String) -> Result<Value, VmError> {
        if operation_id.is_empty() {
            return Err(VmError::Host(
                "host operation id must not be empty".to_owned(),
            ));
        }
        if self.operations.contains_key(&operation_id) {
            return Err(VmError::DuplicateOperation(operation_id));
        }
        let promise = self.heap.alloc_promise();
        self.operations.insert(
            operation_id.clone(),
            crate::runtime::PendingOperation {
                meta: crate::host::PendingOperationMeta {
                    id: operation_id,
                    import,
                },
                promise,
            },
        );
        Ok(Value::Object(promise))
    }

    fn suspend(
        &mut self,
        host: &mut dyn Host,
        frame: &Frame,
        input: Slot,
        resume: u32,
    ) -> Result<(), VmError> {
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
        let prepared = self.prepare_await(host, input_value)?;
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
        self.schedule_resume(continuation_id, waiting_on, plain)
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
            return Value::String("function".to_owned());
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
