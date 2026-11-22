//! //! Iterator and for-in protocol opcodes.

use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::object_id;
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::Value;
use crate::vm::Vm;

use super::Step;

impl Vm {
    pub(super) fn step_iterator(
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
                let invocation = match argument {
                    Some(argument) => {
                        let argument = frame.read(argument)?;
                        self.invoke(
                            host,
                            frame_roots,
                            &method,
                            Arguments::Values(&[argument]),
                            &iterator,
                        )?
                    }
                    None => self.invoke(host, frame_roots, &method, Arguments::EMPTY, &iterator)?,
                };
                let result = match self.require_iterator_object(
                    frame_roots,
                    invocation,
                    "iterator method must return an object",
                )? {
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
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
