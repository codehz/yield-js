//! Typed host instances. The class marker is compile-time only; the VM brand
//! is checked when reading slots or payload.

use crate::binding::{HostClass, JsError, JsResult};
use crate::heap::Attributes;
use crate::value::{ObjectId, PropertyKey, Value};

use super::{CallCx, JsValue};

/// A typed instance allocated by the VM. The marker only exists at compile
/// time; the class tag is checked when reading slots or payload.
pub struct Instance<T: HostClass> {
    id: ObjectId,
    marker: std::marker::PhantomData<T>,
}

impl<T: HostClass> Clone for Instance<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: HostClass> Copy for Instance<T> {}

impl<T: HostClass> Instance<T> {
    pub(crate) fn new(id: ObjectId) -> Self {
        Self {
            id,
            marker: std::marker::PhantomData,
        }
    }

    pub fn value(&self, cx: &CallCx<'_>) -> JsValue {
        cx.js_value(Value::object(self.id))
    }

    pub fn identity(&self) -> u64 {
        self.id as u64
    }

    /// Initialize an own ordinary object field on a newly allocated instance.
    /// This does not invoke inherited setters or replace an existing field.
    pub fn init_object_field(&self, cx: &mut CallCx<'_>, name: &str) -> JsResult<()> {
        let key = PropertyKey::from(name);
        let object = cx
            .vm
            .heap
            .get(self.id)
            .map_err(|error| JsError::fault(error.to_string()))?;
        if object.own_property(key.borrowed()).is_some() {
            return Err(JsError::fault(format!(
                "host instance already has field {name}"
            )));
        }
        let value = cx.vm.heap.alloc_plain();
        cx.vm
            .heap
            .define_data(self.id, key, Value::object(value), Attributes::ORDINARY)
            .map_err(|error| JsError::fault(error.to_string()))
    }

    pub fn slot(&self, cx: &CallCx<'_>, index: usize) -> JsResult<JsValue> {
        cx.slot_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn set_slot(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.set_slot_value(self.id, index, value)
    }

    pub fn list_len(&self, cx: &CallCx<'_>, index: usize) -> JsResult<usize> {
        cx.list_len_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn list_get(&self, cx: &CallCx<'_>, index: usize, position: usize) -> JsResult<JsValue> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_get_value(self.id, index, position)
    }

    pub fn list_push(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_push_value(self.id, index, value)
    }

    pub fn list_clear(&self, cx: &mut CallCx<'_>, index: usize) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_clear_value(self.id, index)
    }

    /// Remove a host instance from an unordered managed list.
    pub fn list_remove_instance<U: HostClass>(
        &self,
        cx: &mut CallCx<'_>,
        index: usize,
        value: Instance<U>,
    ) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        let instance = cx
            .vm
            .host_instances
            .get_mut(&self.id)
            .ok_or_else(|| JsError::fault("value is not a host instance"))?;
        let Some(crate::runtime::HostSlot::JsList(values)) = instance.slots.get_mut(index) else {
            return Err(JsError::fault("slot is not a managed list"));
        };
        if let Some(position) = values
            .iter()
            .position(|item| *item == Value::object(value.id))
        {
            values.swap_remove(position);
        }
        Ok(())
    }

    pub fn payload<'a>(&self, cx: &'a CallCx<'_>) -> JsResult<&'a T> {
        cx.payload_value::<T>(self.id)
    }
}

pub struct This<T: HostClass> {
    pub(super) instance: Instance<T>,
}

impl<T: HostClass> This<T> {
    pub fn instance(&self) -> Instance<T> {
        self.instance
    }
}
