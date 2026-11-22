//! Object statics: extension control, prototype links, and integrity levels.

use super::UNDEFINED_VALUE;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    BoxedPrimitive, DescriptorUpdate, IntegrityLevel, Object, PlainObject, Property,
};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;
impl Vm {
    pub(crate) fn construct_object(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        let value = args.first().unwrap_or(&UNDEFINED_VALUE);
        match value.view() {
            ValueRef::Undefined | ValueRef::Null => {
                Ok(Invocation::Value(Value::object(self.heap.alloc_plain())))
            }
            _ => Ok(Invocation::Value(Value::object(
                self.coercible_object(value),
            ))),
        }
    }

    /// Materialize an already validated ToObject operand, preserving object identity.
    pub(crate) fn coercible_object(&mut self, value: &Value) -> ObjectId {
        let primitive = match value.view() {
            ValueRef::Object(id) => return id,
            ValueRef::String(text) => BoxedPrimitive::String(text.clone()),
            ValueRef::Number(number) => BoxedPrimitive::Number(number),
            ValueRef::BigInt(value) => BoxedPrimitive::BigInt(value.clone()),
            ValueRef::Bool(value) => BoxedPrimitive::Bool(value),
            ValueRef::Symbol(id) => BoxedPrimitive::Symbol(id),
            _ => unreachable!("ToObject requires a non-nullish operand"),
        };
        self.heap.alloc(Object::Boxed {
            primitive,
            object: PlainObject::new(self.primitive_prototype(value)),
        })
    }

    pub(crate) fn primitive_prototype(&self, value: &Value) -> Option<ObjectId> {
        match value.view() {
            ValueRef::String(_) => Some(self.builtins.get("string.prototype")),
            ValueRef::Number(_) => Some(self.builtins.get("number.prototype")),
            ValueRef::BigInt(_) => Some(self.builtins.get("bigint.prototype")),
            ValueRef::Bool(_) => Some(self.builtins.get("boolean.prototype")),
            ValueRef::Symbol(_) => Some(self.builtins.get("symbol.prototype")),
            _ => None,
        }
    }

    pub(crate) fn object_create(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let prototype = match args.first().map(Value::view) {
            Some(ValueRef::Null) => None,
            Some(ValueRef::Object(id)) => Some(id),
            _ => {
                return self
                    .throw_type_error(roots, "Object.create requires an object or null prototype");
            }
        };
        let object = self.heap.alloc_plain_with(prototype);
        match args
            .get(1)
            .filter(|value| !matches!(value.view(), ValueRef::Undefined))
        {
            None => Ok(Invocation::Value(Value::object(object))),
            properties => self.define_properties(host, roots, object, properties),
        }
    }

    pub(crate) fn prevent_extensions_builtin_impl(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if let ValueRef::Object(id) = target.view() {
            let id = self.heap.proxy_target(id)?;
            // A view over a resizable buffer can still gain and lose elements,
            // so it cannot be made non-extensible.
            if self
                .heap
                .typed_array(id)?
                .is_some_and(|view| view.byte_length.is_none())
            {
                return if reflect {
                    Ok(Invocation::Value(Value::boolean(false)))
                } else {
                    self.throw_type_error(
                        roots,
                        "cannot prevent extensions on a length-tracking TypedArray",
                    )
                };
            }
            self.heap.get_mut(id)?.prevent_extensions();
        } else if reflect {
            return self.throw_type_error(roots, "Reflect.preventExtensions requires an object");
        }
        Ok(Invocation::Value(if reflect {
            Value::boolean(true)
        } else {
            target.clone()
        }))
    }

    pub(crate) fn is_extensible_builtin_impl(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let extensible = match args.first().map(Value::view) {
            Some(ValueRef::Object(id)) => {
                self.heap.get(self.heap.proxy_target(id)?)?.is_extensible()
            }
            _ if reflect => {
                return self.throw_type_error(roots, "Reflect.isExtensible requires an object");
            }
            _ => false,
        };
        Ok(Invocation::Value(Value::boolean(extensible)))
    }

    pub(crate) fn get_prototype_of(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            if !reflect
                && let Some(prototype) = args
                    .first()
                    .and_then(|value| self.primitive_prototype(value))
            {
                return Ok(Invocation::Value(Value::object(prototype)));
            }
            return self.throw_type_error(
                roots,
                if reflect {
                    "Reflect.getPrototypeOf requires an object"
                } else {
                    "Object.getPrototypeOf cannot convert null or undefined to an object"
                },
            );
        };
        Ok(Invocation::Value(
            self.heap
                .get(self.heap.proxy_target(target)?)?
                .prototype()
                .map_or(Value::NULL, Value::object),
        ))
    }

    pub(crate) fn set_prototype_of(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if reflect && !matches!(target.view(), ValueRef::Object(_)) {
            return self
                .throw_type_error(roots, "Reflect.setPrototypeOf requires an object target");
        }
        if matches!(target.view(), ValueRef::Undefined | ValueRef::Null) {
            return self
                .throw_type_error(roots, "Object.setPrototypeOf requires a non-nullish target");
        }
        let prototype = match args.get(1).map(Value::view) {
            Some(ValueRef::Null) => None,
            Some(ValueRef::Object(id)) => Some(id),
            _ => {
                return self.throw_type_error(
                    roots,
                    "setPrototypeOf requires an object or null prototype",
                );
            }
        };
        let ValueRef::Object(target_id) = target.view() else {
            return Ok(Invocation::Value(target.clone()));
        };
        let accepted = self.heap.set_prototype(target_id, prototype)?;
        if reflect {
            Ok(Invocation::Value(Value::boolean(accepted)))
        } else if accepted {
            Ok(Invocation::Value(Value::object(target_id)))
        } else {
            self.throw_type_error(roots, "Object.setPrototypeOf cannot set the prototype")
        }
    }

    pub(crate) fn set_integrity(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        level: IntegrityLevel,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        let ValueRef::Object(id) = target.view() else {
            return Ok(Invocation::Value(target.clone()));
        };
        let base = self.heap.proxy_target(id)?;
        // A typed array's elements are defined one by one, so a non-empty view
        // cannot be frozen; the per-key path reports that as a TypeError.
        if base == id && self.heap.typed_array(id)?.is_none() {
            self.heap.get_mut(id)?.set_integrity_level(level);
            return Ok(Invocation::Value(target.clone()));
        }
        self.heap.get_mut(base)?.prevent_extensions();
        let keys = match self.own_keys(host, roots, id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        for key in &keys {
            let writable = if matches!(level, IntegrityLevel::Frozen) {
                let roots = &roots.with(&keys);
                match self.own_descriptor(host, roots, id, key.borrowed())? {
                    Ok(Some((Property::Data(_), _))) => Some(false),
                    Ok(Some(_)) => None,
                    Ok(None) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            } else {
                None
            };
            let update = DescriptorUpdate {
                configurable: Some(false),
                writable,
                ..DescriptorUpdate::default()
            };
            if let thrown @ Invocation::Thrown(_) =
                self.apply_property_update(host, roots, id, key.clone(), update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(target.clone()))
    }

    pub(crate) fn test_integrity(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        level: IntegrityLevel,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let result = |value| Ok(Invocation::Value(Value::boolean(value)));
        let Some(ValueRef::Object(id)) = args.first().map(Value::view) else {
            return result(true);
        };
        let base = self.heap.proxy_target(id)?;
        if base == id {
            return result(self.heap.get(base)?.test_integrity_level(level));
        }
        if self.heap.get(base)?.is_extensible() {
            return result(false);
        }
        let keys = match self.own_keys(host, roots, id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        for key in &keys {
            let roots = &roots.with(&keys);
            match self.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(Some((property, attributes)))
                    if attributes.configurable
                        || (matches!(level, IntegrityLevel::Frozen)
                            && matches!(property, Property::Data(_))
                            && attributes.writable) =>
                {
                    return result(false);
                }
                Ok(_) => {}
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        result(true)
    }
}
