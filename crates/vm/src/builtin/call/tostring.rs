//! Array and object stringification, including Symbol.toStringTag lookup.

use super::UNDEFINED_VALUE;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{BoxedPrimitive, Object, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
impl Vm {
    pub(crate) fn array_to_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(
                roots,
                "Array.prototype.toString called on null or undefined",
            );
        }
        let roots = &roots.with(receiver);
        let mut boxed = None;
        let join = match self.read_to_object_property(
            host,
            roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("join"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        if self.is_callable(&join) {
            let receiver = Value::object(boxed.unwrap_or_else(|| self.coercible_object(receiver)));
            self.invoke(host, roots, &join, Arguments::EMPTY, &receiver)
        } else {
            match boxed {
                Some(id) => self.object_to_string(host, roots, &Value::object(id)),
                None => self.object_to_string(host, roots, receiver),
            }
        }
    }

    pub(crate) fn object_to_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let tag = match receiver.view() {
            ValueRef::Object(id) if self.heap.is_environment(id) == Some(true) => {
                return self.throw_type_error(roots, "incompatible Object.prototype receiver");
            }
            ValueRef::Object(id) => {
                let target = match self.proxy_base(roots, id)? {
                    Ok(target) => target,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                self.object_tag(id, target)?
            }
            ValueRef::Undefined => {
                return Ok(Invocation::Value(Value::string(
                    "[object Undefined]".into(),
                )));
            }
            ValueRef::Null => {
                return Ok(Invocation::Value(Value::string("[object Null]".into())));
            }
            ValueRef::String(_) => "String",
            ValueRef::Number(_) => "Number",
            ValueRef::BigInt(_) => "BigInt",
            ValueRef::Bool(_) => "Boolean",
            ValueRef::Symbol(_) => "Symbol",
        };
        let custom_tag = match self.read_to_object_property(
            host,
            &roots.with(receiver),
            receiver,
            &mut None,
            PropertyKeyRef::Symbol(TO_STRING_TAG_SYMBOL),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        if let ValueRef::String(custom_tag) = custom_tag.view() {
            let mut result = JsString::from("[object ");
            result.push_str(custom_tag);
            result.push(']');
            return Ok(Invocation::Value(Value::string(result)));
        }
        Ok(Invocation::Value(Value::string(
            format!("[object {tag}]").into(),
        )))
    }

    fn object_tag(&self, id: ObjectId, target: ObjectId) -> Result<&'static str, VmError> {
        Ok(match self.heap.get(target)? {
            Object::Date { .. } if id == target => "Date",
            Object::DomException { .. } if id == target => "Error",
            Object::Boxed { primitive, .. } => match primitive {
                BoxedPrimitive::String(_) => "String",
                BoxedPrimitive::Number(_) => "Number",
                BoxedPrimitive::BigInt(_) => "BigInt",
                BoxedPrimitive::Bool(_) => "Boolean",
                BoxedPrimitive::Symbol(_) => "Symbol",
            },
            Object::Array(_) => "Array",
            Object::Function(_)
            | Object::Native(_)
            | Object::Bound(_)
            | Object::PromiseCallback { .. }
            | Object::ProxyRevoker { .. }
            | Object::Resolution(_) => "Function",
            Object::Promise(_) => "Promise",
            Object::Map { .. } => "Map",
            Object::Set { .. } => "Set",
            _ => "Object",
        })
    }

    pub(crate) fn array_join(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self
                .throw_type_error(roots, "Array.prototype.join called on null or undefined");
        }
        let mut boxed = None;
        let length = match self.read_to_object_property(
            host,
            roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("length"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        let (length, separator) = {
            let roots = &roots.with(&boxed);
            let length = match self.coerce_number(host, roots, &length)? {
                Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let separator = match args.first().unwrap_or(&UNDEFINED_VALUE) {
                value if matches!(value.view(), ValueRef::Undefined) => ",".into(),
                value => match self.coerce_string(host, roots, value)? {
                    Ok(string) => string,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
            };
            (length, separator)
        };
        if let ValueRef::String(text) = receiver.view() {
            // Every index below the string's fixed length is an own data property.
            if separator.is_empty() {
                return Ok(Invocation::Value(receiver.clone()));
            }
            let mut output = JsString::default();
            for (index, unit) in text.units().enumerate() {
                if index > 0 {
                    output.push_str(&separator);
                }
                output.push_unit(unit);
            }
            return Ok(Invocation::Value(Value::string(output)));
        }
        let array = match receiver.view() {
            ValueRef::Object(id) if matches!(self.heap.get(id)?, Object::Array(_)) => Some(id),
            _ => None,
        };
        if let Some(id) = array
            && !self.active_array_joins.insert(id)
        {
            return Ok(Invocation::Value(Value::string(JsString::default())));
        }
        let result = (|| {
            let mut output = JsString::default();
            for index in 0..length {
                if index > 0 {
                    output.push_str(&separator);
                }
                let item = if let Some(id) = array {
                    self.read_array_index(host, roots, id, index as usize)?
                } else {
                    self.read_to_object_property(
                        host,
                        roots,
                        receiver,
                        &mut boxed,
                        PropertyKeyRef::String(&index.to_string()),
                    )?
                };
                let value = match item {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) {
                    continue;
                }
                let roots = &roots.with(&boxed);
                match self.coerce_string(host, roots, &value)? {
                    Ok(value) => output.push_str(&value),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            Ok(Invocation::Value(Value::string(output)))
        })();
        if let Some(id) = array {
            self.active_array_joins.remove(&id);
        }
        result
    }
}
