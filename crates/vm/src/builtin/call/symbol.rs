//! Symbol construction and well-known symbol registration helpers.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{BoxedPrimitive, Object};
use crate::host::{Host, PlatformServices};
use crate::runtime::Invocation;
use crate::value::{JsString, Value, ValueRef};
use crate::vm::VmCore;
impl VmCore {
    pub(crate) fn symbol_static(
        &mut self,
        _receiver: &Value,
        call: impl FnOnce(&mut Self, Vec<Value>) -> Result<Invocation, VmError>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        // `Symbol.for` and `Symbol.keyFor` do not read `this`, so an extracted
        // method reference is callable in real engines and in this VM.
        call(self, args)
    }

    pub(crate) fn symbol_method(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        method: &str,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Symbol(id) => id,
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::Symbol(id),
                    ..
                } => *id,
                _ => return self.throw_type_error(roots, "incompatible Symbol receiver"),
            },
            _ => return self.throw_type_error(roots, "incompatible Symbol receiver"),
        };
        let description = self.heap.symbols.get(&id).and_then(Option::as_ref);
        Ok(Invocation::Value(match method {
            "valueOf" => Value::symbol(id),
            "toString" => {
                let mut text = JsString::from("Symbol(");
                if let Some(description) = description {
                    text.push_str(description);
                }
                text.push(')');
                Value::string(text)
            }
            "get description" => description.cloned().map_or(Value::UNDEFINED, Value::string),
            _ => unreachable!("unknown Symbol method"),
        }))
    }

    /// `Symbol(desc)`: every call produces a fresh identity.
    pub(crate) fn symbol_constructor<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let description = match args.first().unwrap_or(&Value::UNDEFINED) {
            value if matches!(value.view(), ValueRef::Undefined) => None,
            value => match self.coerce_string(host, roots, value)? {
                Ok(description) => Some(description),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let id = self.heap.alloc_symbol(description)?;
        Ok(Invocation::Value(Value::symbol(id)))
    }

    pub(crate) fn symbol_for<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let key =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let id = self.heap.intern_symbol(key)?;
        Ok(Invocation::Value(Value::symbol(id)))
    }

    pub(crate) fn symbol_key_for(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let Some(ValueRef::Symbol(id)) = args.first().map(Value::view) else {
            return self.throw_type_error(roots, "Symbol.keyFor requires a Symbol");
        };
        let key = self
            .heap
            .symbol_registry
            .iter()
            .find_map(|(key, registered)| (*registered == id).then(|| key.clone()));
        Ok(Invocation::Value(match key {
            Some(key) => Value::string(key),
            None => Value::UNDEFINED,
        }))
    }
}
