//! //! Calls, constructors, super access, and suspension entry points.

use super::{Step, converted_property_key, store_invocation};
use crate::bytecode::{CallArgs, Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, object_id};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::{Value, ValueRef};
use crate::vm::Vm;

impl Vm {
    pub(super) fn step_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        _constants: &[Constant],
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
            Instr::CallValue {
                dst,
                callee,
                ref args,
                receiver,
            } => self.call_value(host, roots, frame, dst, callee, args, receiver),
            Instr::Construct {
                dst,
                callee,
                ref args,
            } => {
                let callee = frame.read(callee)?;
                let invocation = match args {
                    CallArgs::Slots(slots) => self.construct_with(
                        host,
                        frame_roots,
                        &callee,
                        Arguments::Slots(frame, slots),
                        &callee,
                    )?,
                    CallArgs::Spread(slot) => {
                        let values = self.spread_arguments(frame, *slot)?;
                        self.construct_with(
                            host,
                            frame_roots,
                            &callee,
                            Arguments::Values(&values),
                            &callee,
                        )?
                    }
                };
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
                    return Ok(Step::Throw(
                        self.type_error(frame_roots, "super base is null")?,
                    ));
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
                    return Ok(Step::Throw(
                        self.type_error(frame_roots, "super base is null")?,
                    ));
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
                let invocation = self.derived_this(frame_roots, context)?;
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
                ref args,
            } => {
                let context = object_id(frame.read_ref(context)?, "super construction context")?;
                let invocation = match args {
                    CallArgs::Slots(slots) => self.super_construct(
                        host,
                        frame_roots,
                        context,
                        frame.read_ref(callee)?,
                        Arguments::Slots(frame, slots),
                    )?,
                    CallArgs::Spread(slot) => {
                        let values = self.spread_arguments(frame, *slot)?;
                        self.super_construct(
                            host,
                            frame_roots,
                            context,
                            frame.read_ref(callee)?,
                            Arguments::Values(&values),
                        )?
                    }
                };
                store_invocation(frame, dst, invocation)
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
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
