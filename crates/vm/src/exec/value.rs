//! //! Slot-to-slot value operations: moves, operators, coercions, and throws.

use super::store_invocation;
use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::host::{Host, PlatformServices};
use crate::value::{PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

use super::Step;

impl VmCore {
    pub(super) fn step_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        constants: &[Constant],
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
            Instr::Const { dst, constant } => {
                frame.write(dst, Self::constant(constants, constant)?)?;
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
                let lhs = frame.read(lhs)?;
                let rhs = frame.read(rhs)?;
                match self.binary_value(host, frame_roots, op, lhs, rhs)? {
                    Ok(value) => {
                        frame.write(dst, value)?;
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::BinaryConst {
                dst,
                op,
                lhs,
                constant,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = Self::constant(constants, constant)?;
                match self.binary_value(host, frame_roots, op, lhs, rhs)? {
                    Ok(value) => {
                        frame.write(dst, value)?;
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::Unary { dst, op, src } => {
                let value = frame.read(src)?;
                match self.unary_value(host, frame_roots, op, value)? {
                    Ok(value) => {
                        frame.write(dst, value)?;
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
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
                        self.type_error(frame_roots, "property access on a non-object")?,
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
            Instr::Return { src } => Ok(Step::Return(frame.read(src)?)),
            Instr::Throw { src } => Ok(Step::Throw(frame.read(src)?)),
            Instr::ThrowDeleteSuper => Ok(Step::Throw(
                self.reference_error(frame_roots, "cannot delete a super property")?,
            )),
            Instr::ThrowAssignmentToConst => Ok(Step::Throw(
                self.type_error(frame_roots, "Assignment to constant variable.")?,
            )),
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
