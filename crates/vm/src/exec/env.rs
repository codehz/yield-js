//! //! Environment, module, builtin, and template reads.

use std::collections::BTreeMap;

use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{Object, object_id};
use crate::host::Host;
use crate::value::Value;
use crate::vm::Vm;

use super::Step;

impl Vm {
    pub(super) fn step_env(
        &mut self,
        _host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        _constants: &[Constant],
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
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
                    return Ok(Step::Throw(self.reference_error(
                        frame_roots,
                        &format!("module binding {binding} is not initialized"),
                    )?));
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::GetModuleNamespace { dst, module } => {
                frame.write(dst, self.module_namespace(module)?)?;
                Ok(Step::Next)
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
            Instr::GetBindingDirect {
                dst,
                depth,
                binding,
            } => {
                let env = frame
                    .env
                    .ok_or_else(|| VmError::InvalidState("binding read without env".to_owned()))?;
                frame.write(dst, self.heap.get_binding_direct(env, depth, binding)?)?;
                Ok(Step::Next)
            }
            Instr::SetBindingDirect {
                depth,
                binding,
                src,
            } => {
                let env = frame
                    .env
                    .ok_or_else(|| VmError::InvalidState("binding write without env".to_owned()))?;
                self.heap
                    .set_binding_direct(env, depth, binding, frame.read(src)?)?;
                Ok(Step::Next)
            }
            Instr::GetModuleBindingDirect {
                dst,
                module,
                binding,
            } => {
                let env = self.module_environment(module)?;
                frame.write(dst, self.heap.get_module_binding_direct(env, binding)?)?;
                Ok(Step::Next)
            }
            Instr::SetModuleBindingDirect {
                module,
                binding,
                src,
            } => {
                let env = self.module_environment(module)?;
                let initialize = frame.env == Some(env);
                self.heap
                    .set_module_binding_direct(env, binding, frame.read(src)?, initialize)?;
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
            Instr::NewIterationEnv {
                parent,
                ref bindings,
            } => {
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
                for &binding in bindings {
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
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
