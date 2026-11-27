//! Host instance slot, list, and payload access. Slot kinds are stable for the
//! life of an instance; weak slots do not keep their targets alive.

use crate::binding::{HostClass, HostFault, JsError, JsResult};
use crate::heap::WeakKey;
use crate::value::ObjectId;

use super::{CallCx, JsValue};

impl CallCx<'_> {
    pub(super) fn weak_slot_js(&self, key: Option<WeakKey>) -> JsValue {
        match key {
            None => JsValue::undefined(),
            Some(key) => self.js_value(key.to_value()),
        }
    }

    pub(super) fn decode_weak_slot(&mut self, value: &JsValue) -> JsResult<Option<WeakKey>> {
        self.check_value(value)?;
        if value.is_undefined() {
            return Ok(None);
        }
        self.vm
            .heap
            .weak_key(&value.value)
            .ok_or_else(|| self.type_error("invalid weak slot target"))
            .map(Some)
    }

    fn instance_record(&self, id: ObjectId) -> JsResult<&crate::runtime::HostInstance> {
        self.vm
            .host_instances
            .get(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))
    }

    pub(crate) fn slot_value(&self, id: ObjectId, index: usize) -> JsResult<JsValue> {
        let instance = self.instance_record(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsWeak(key)) => Ok(self.weak_slot_js(*key)),
            Some(crate::runtime::HostSlot::JsList(_)) => {
                Err(JsError::Fault(HostFault::new("slot is a JavaScript list")))
            }
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn set_slot_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        let tag = {
            let instance =
                self.vm.host_instances.get(&id).ok_or_else(|| {
                    JsError::Fault(HostFault::new("value is not a host instance"))
                })?;
            match instance.slots.get(index) {
                Some(crate::runtime::HostSlot::Js(_)) => 0u8,
                Some(crate::runtime::HostSlot::JsWeak(_)) => 1,
                Some(crate::runtime::HostSlot::JsList(_)) => 2,
                None => {
                    return Err(JsError::Fault(HostFault::new("slot index out of range")));
                }
            }
        };
        match tag {
            0 => {
                self.check_value(&value)?;
                let crate::runtime::HostSlot::Js(slot) =
                    &mut self.vm.host_instances.get_mut(&id).unwrap().slots[index]
                else {
                    unreachable!("slot kind is stable")
                };
                *slot = value.value;
                Ok(())
            }
            1 => {
                let key = self.decode_weak_slot(&value)?;
                let crate::runtime::HostSlot::JsWeak(slot) =
                    &mut self.vm.host_instances.get_mut(&id).unwrap().slots[index]
                else {
                    unreachable!("slot kind is stable")
                };
                *slot = key;
                Ok(())
            }
            _ => Err(JsError::Fault(HostFault::new("slot is a JavaScript list"))),
        }
    }

    pub(crate) fn list_len_value(&self, id: ObjectId, index: usize) -> JsResult<usize> {
        let instance = self.instance_record(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("slot is a single JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_get_value(
        &self,
        id: ObjectId,
        index: usize,
        position: usize,
    ) -> JsResult<JsValue> {
        let instance = self.instance_record(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => values
                .get(position)
                .cloned()
                .map(|value| self.js_value(value))
                .ok_or_else(|| JsError::Fault(HostFault::new("list index out of range"))),
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("slot is a single JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_push_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let value = value.value;
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("slot is a single JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_clear_value(&mut self, id: ObjectId, index: usize) -> JsResult<()> {
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.clear();
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_) | crate::runtime::HostSlot::JsWeak(_)) => Err(
                JsError::Fault(HostFault::new("slot is a single JavaScript value")),
            ),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn payload_value<T: HostClass>(&self, id: ObjectId) -> JsResult<&T> {
        let class = self.bindings.class_by_tag(T::TAG).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!("unregistered class {}", T::TAG)))
        })?;
        let instance = self.instance_record(id)?;
        if instance.class != class {
            return Err(JsError::Fault(HostFault::new(format!(
                "instance is not a {}",
                T::TAG
            ))));
        }
        instance
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<T>())
            .ok_or_else(|| JsError::Fault(HostFault::new(format!("{} has no payload", T::TAG))))
    }
}
