//! //! Ordinary object and property operations, including the SetMember write cache.

use std::collections::BTreeSet;

use super::{Step, converted_property_key, store_invocation};
use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{Object, object_id};
use crate::host::{Host, PlatformServices};
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

impl VmCore {
    pub(super) fn step_object<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        constants: &[Constant],
        pc: usize,
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
            Instr::NewObject { dst } => {
                let id = self.heap.alloc_plain();
                frame.write(dst, Value::object(id))?;
                Ok(Step::Next)
            }
            Instr::NewRegExp {
                dst,
                ref source,
                ref flags,
            } => {
                let id = self.alloc_regexp(source, flags)?;
                frame.write(dst, Value::object(id))?;
                Ok(Step::Next)
            }
            Instr::NewArray { dst, ref elements } => {
                let elements = frame.read_list(elements)?;
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
            Instr::GetMemberConstKey { dst, object, key } => {
                let Some(Constant::String(key)) = constants.get(key as usize) else {
                    return Err(VmError::InvalidState(format!(
                        "GetMemberConstKey requires a string constant at index {key}"
                    )));
                };
                let invocation = self.read_property(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    PropertyKeyRef::JsString(key),
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
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "cannot delete a non-configurable property",
                    )?));
                }
                store_invocation(frame, dst, invocation)
            }
            Instr::SetMember { object, key, src } => {
                let site = (frame.function, pc);
                match self.write_member(
                    host,
                    frame_roots,
                    &frame.read(object)?,
                    &frame.read(key)?,
                    frame.read(src)?,
                    Some(site),
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
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "Cannot destructure null or undefined",
                    )?));
                }
                Ok(Step::Next)
            }
            Instr::RequireObject { src } => {
                if matches!(frame.read_ref(src)?.view(), ValueRef::Object(_)) {
                    Ok(Step::Next)
                } else {
                    Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "async iterator must be an object",
                    )?))
                }
            }
            Instr::ObjectRest {
                dst,
                src,
                ref excluded,
            } => {
                let source = frame.read_ref(src)?;
                if matches!(source.view(), ValueRef::Null | ValueRef::Undefined) {
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "Cannot destructure null or undefined",
                    )?));
                }
                let excluded = excluded
                    .iter()
                    .copied()
                    .map(|slot| converted_property_key(frame.read_ref(slot)?))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let target = self.heap.alloc_plain();
                match self.copy_data_properties(host, frame_roots, target, source, &excluded)? {
                    Invocation::Value(_) => frame.write(dst, Value::object(target))?,
                    Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
