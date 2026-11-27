//! Host instance allocation and branding. Intrinsic initialization may run
//! guest conversions; event-parent callbacks cannot allocate standard bases.

use crate::binding::{HostClass, HostFault, JsError, JsResult, SlotKind};
use crate::heap::{EventTargetState, Object, PlainObject};
use crate::intrinsic::{ClassArgs, ClassDef};
use crate::intrinsics::IntrinsicClass;
use crate::value::{ObjectId, Value, ValueRef};

use super::{CallCx, Instance, JsValue, This};

impl CallCx<'_> {
    /// Construct an intrinsic with the current call's active stack.
    /// Argument conversion may run guest code and throw its original value.
    pub fn construct<C: IntrinsicClass>(&mut self, args: &[JsValue]) -> JsResult<JsValue> {
        let class = C::CLASS;
        let value = self.initialize_intrinsic(
            class,
            self.vm.builtins.get(class.prototype),
            class.key.owner,
            args,
        )?;
        Ok(self.js_value(value))
    }

    pub(super) fn initialize_intrinsic(
        &mut self,
        class: &'static ClassDef,
        prototype: ObjectId,
        name: &str,
        args: &[JsValue],
    ) -> JsResult<Value> {
        // Intrinsic initialization can invoke guest conversions/getters, even
        // when reached indirectly through alloc_with_super.
        let initialize_class = self.initialize_class.ok_or_else(|| {
            JsError::fault("event parent callbacks cannot initialize intrinsic classes")
        })?;
        for arg in args {
            self.check_value(arg)?;
        }
        let live = (&self.handles, self.args);
        let result = initialize_class(
            self.vm,
            &self.roots.with(&live),
            class,
            prototype,
            name,
            ClassArgs::Handles(args),
        )
        .map_err(|error| JsError::fault(error.to_string()))?;
        match result {
            crate::runtime::Invocation::Value(value) => Ok(value),
            crate::runtime::Invocation::Thrown(value) => Err(self.throw(self.js_value(value))),
        }
    }

    pub fn alloc<T: HostClass>(&mut self, payload: T) -> JsResult<Instance<T>> {
        self.alloc_instance(payload, None)
    }

    /// Pass explicit arguments to the declared standard base constructor.
    /// Conversion may execute guest code and throw the original JavaScript value.
    pub fn alloc_with_super<T: HostClass>(
        &mut self,
        payload: T,
        args: &[JsValue],
    ) -> JsResult<Instance<T>> {
        self.alloc_instance(payload, Some(args))
    }

    fn alloc_instance<T: HostClass>(
        &mut self,
        payload: T,
        args: Option<&[JsValue]>,
    ) -> JsResult<Instance<T>> {
        let class = self
            .bindings
            .class_by_tag(T::TAG)
            .ok_or_else(|| JsError::Fault(self.fault(format!("unregistered class {}", T::TAG))))?;
        let builtin = self.bindings.class_intrinsic_base(class);
        if args.is_some() && builtin.is_none() {
            return Err(JsError::fault(format!(
                "class {} has no standard base",
                T::TAG
            )));
        }
        let record = self.vm.host_classes.get(class).ok_or_else(|| {
            JsError::Fault(self.fault(format!("class {} has no prototype", T::TAG)))
        })?;
        let prototype = self
            .allocation_prototype
            .filter(|(target, _)| *target == record.source)
            .map_or(record.prototype, |(_, prototype)| prototype);
        let payload = Box::new(payload);
        if !self.bindings.class_has_payload_codec(class)
            && !self
                .bindings
                .class_payload_is_empty(class, payload.as_ref())
        {
            return Err(JsError::Fault(HostFault::new(format!(
                "class {} has data but no payload codec",
                T::TAG
            ))));
        }
        let object = if let Some(builtin) = builtin {
            let name = self
                .bindings
                .class_error_stack_name(class, builtin.key.owner);
            let value =
                self.initialize_intrinsic(builtin, prototype, name, args.unwrap_or_default())?;
            let ValueRef::Object(id) = value.view() else {
                unreachable!("class initializer must allocate an object")
            };
            id
        } else if self.bindings.class_is_event_target(class) {
            // EventTarget branding is independent of standard exception bases.
            self.vm.heap.alloc(Object::EventTarget {
                object: PlainObject::new(Some(prototype)),
                target: EventTargetState::default(),
            })
        } else {
            self.vm.heap.alloc_plain_with(Some(prototype))
        };
        let slots = self
            .bindings
            .class_slots(class)
            .iter()
            .map(|slot| match slot.kind {
                SlotKind::Js => crate::runtime::HostSlot::Js(Value::UNDEFINED),
                SlotKind::JsList => crate::runtime::HostSlot::JsList(Vec::new()),
                SlotKind::JsWeak => crate::runtime::HostSlot::JsWeak(None),
            });
        let slots = slots.collect();
        self.vm.host_instances.insert(
            object,
            crate::runtime::HostInstance {
                class,
                slots,
                payload: Some(payload),
            },
        );
        self.handles.get_mut().retain(&Value::object(object));
        Ok(Instance::new(object))
    }

    pub fn this<T: HostClass>(&mut self) -> JsResult<This<T>> {
        let id = Self::instance_id(&self.this);
        Ok(This {
            instance: self.branded_instance(id)?,
        })
    }

    /// Resolve an existing host instance by its VM brand, including subclasses.
    pub fn instance<T: HostClass>(&mut self, value: &JsValue) -> JsResult<Instance<T>> {
        self.check_value(value)?;
        self.branded_instance(Self::instance_id(value))
    }

    /// Resolve a VM-local identity previously obtained from a host instance.
    pub fn instance_by_identity<T: HostClass>(&mut self, id: ObjectId) -> JsResult<Instance<T>> {
        self.branded_instance(Some(id))
    }

    fn instance_id(value: &JsValue) -> Option<ObjectId> {
        match value.value.view() {
            ValueRef::Object(id) => Some(id),
            _ => None,
        }
    }

    fn branded_instance<T: HostClass>(&mut self, id: Option<ObjectId>) -> JsResult<Instance<T>> {
        let Some(id) = id else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        let Some(class) = self.bindings.class_by_tag(T::TAG) else {
            return Err(JsError::Fault(
                self.fault(format!("unregistered class {}", T::TAG)),
            ));
        };
        let Some(instance) = self.vm.host_instances.get(&id) else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        if !self.bindings.class_is_a(instance.class, class) {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        }
        Ok(Instance::new(id))
    }
}
