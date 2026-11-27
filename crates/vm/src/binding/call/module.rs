//! Typed module and class trampolines. Dispatch checks the owner payload type
//! before handing out [`ModuleCx`] or [`ClassCx`].

use crate::binding::services::Services;
use crate::binding::{HostClass, HostFault, HostModule, HostStep, JsError, JsResult, NativeTarget};

use super::{CallCx, JsValue};

/// Runtime-only input to a typed module trampoline.
#[doc(hidden)]
pub struct ModuleDispatch<'a> {
    call: CallCx<'a>,
    owner: NativeTarget,
}

impl<'a> ModuleDispatch<'a> {
    pub(crate) fn new(call: CallCx<'a>, owner: NativeTarget) -> Self {
        Self { call, owner }
    }
}

#[doc(hidden)]
pub fn with_module<'a, M: HostModule>(
    dispatch: ModuleDispatch<'a>,
    run: impl FnOnce(&mut ModuleCx<'a, M>) -> JsResult<HostStep>,
) -> (CallCx<'a>, JsResult<HostStep>) {
    let ModuleDispatch { call, owner } = dispatch;
    let NativeTarget::Export(export) = owner else {
        return (
            call,
            Err(JsError::fault("typed callback is not a module export")),
        );
    };
    let module = export.module as usize;
    if call.bindings.state_type(module) != Some(std::any::TypeId::of::<M>())
        || !call
            .vm
            .module_states
            .get(module)
            .is_some_and(|state| state.payload.is::<M>())
    {
        return (
            call,
            Err(JsError::fault("module state payload type mismatch")),
        );
    }
    // SAFETY: the module declaration above was checked to belong to M.
    let services = unsafe { call.bindings.state_services(module).for_owner::<M>() };
    let mut cx = ModuleCx {
        call,
        module,
        services,
    };
    let result = run(&mut cx);
    (cx.call, result)
}

/// A host export's capability to access its own per-VM module state.
pub struct ModuleCx<'a, M: HostModule> {
    call: CallCx<'a>,
    module: usize,
    services: Services<'a, M>,
}

impl<'a, M: HostModule> std::ops::Deref for ModuleCx<'a, M> {
    type Target = CallCx<'a>;

    fn deref(&self) -> &Self::Target {
        &self.call
    }
}

impl<M: HostModule> std::ops::DerefMut for ModuleCx<'_, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.call
    }
}

impl<'a, M: HostModule> ModuleCx<'a, M> {
    /// The required services declared by this module.
    pub fn services(&self) -> &Services<'a, M> {
        &self.services
    }
}

impl<M: HostModule> ModuleCx<'_, M> {
    pub fn state(&mut self) -> &mut M {
        self.call.vm.module_states[self.module]
            .payload
            .downcast_mut::<M>()
            .expect("module payload validated before dispatch")
    }

    pub fn state_slot(&self, index: usize) -> JsResult<JsValue> {
        match self.call.vm.module_states[self.module].slots.get(index) {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsWeak(key)) => Ok(self.weak_slot_js(*key)),
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn set_state_slot(&mut self, index: usize, value: JsValue) -> JsResult<()> {
        match self.call.vm.module_states[self.module].slots.get(index) {
            Some(crate::runtime::HostSlot::Js(_)) => {
                self.check_value(&value)?;
                let crate::runtime::HostSlot::Js(slot) =
                    &mut self.call.vm.module_states[self.module].slots[index]
                else {
                    unreachable!("slot kind is stable")
                };
                *slot = value.value;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsWeak(_)) => {
                let key = self.decode_weak_slot(&value)?;
                let crate::runtime::HostSlot::JsWeak(slot) =
                    &mut self.call.vm.module_states[self.module].slots[index]
                else {
                    unreachable!("slot kind is stable")
                };
                *slot = key;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_len(&self, index: usize) -> JsResult<usize> {
        match self.call.vm.module_states[self.module].slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("module state slot is a JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_push(&mut self, index: usize, value: JsValue) -> JsResult<()> {
        self.check_value(&value)?;
        match self.call.vm.module_states[self.module].slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value.value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("module state slot is a JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }
}

/// A class callback with access to the declaring class's linked services.
pub struct ClassCx<'call, 'a, C: HostClass> {
    call: &'call mut CallCx<'a>,
    services: Services<'a, C>,
}

impl<'a, C: HostClass> ClassCx<'_, 'a, C> {
    pub fn services(&self) -> &Services<'a, C> {
        &self.services
    }
}

impl<'a, C: HostClass> std::ops::Deref for ClassCx<'_, 'a, C> {
    type Target = CallCx<'a>;

    fn deref(&self) -> &Self::Target {
        self.call
    }
}

impl<C: HostClass> std::ops::DerefMut for ClassCx<'_, '_, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.call
    }
}

/// Runs a class callback with the declaring class's typed services.
pub fn with_class<'a, C: HostClass, R>(
    call: &mut CallCx<'a>,
    run: impl FnOnce(&mut ClassCx<'_, 'a, C>) -> JsResult<R>,
) -> JsResult<R> {
    let services = call
        .class_owner
        .and_then(|owner| call.bindings.class_callback_services::<C>(owner))
        .ok_or_else(|| JsError::fault("class callback owner mismatch"))?;
    run(&mut ClassCx { call, services })
}
