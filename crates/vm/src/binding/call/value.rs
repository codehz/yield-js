//! Host-boundary values. Object and symbol identity is valid only in the VM
//! generation and call that produced the handle.

use std::cell::RefCell;
use std::fmt;

use crate::binding::{HostFault, JsError, JsResult};
use crate::gc::{TraceRoots, Tracer};
use crate::value::{JsBigInt, JsString, Value, ValueRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Constant,
    Handle { epoch: u32, call: u64 },
}

/// A value crossing the host boundary. Object and symbol identity only make
/// sense within the VM generation and call that produced the handle.
#[derive(Debug, Clone)]
pub struct JsValue {
    pub(crate) value: Value,
    pub(crate) origin: Origin,
}

impl JsValue {
    pub fn undefined() -> Self {
        Self::constant(Value::UNDEFINED)
    }

    pub fn null() -> Self {
        Self::constant(Value::NULL)
    }

    pub fn boolean(value: bool) -> Self {
        Self::constant(Value::boolean(value))
    }

    pub fn number(value: f64) -> Self {
        Self::constant(Value::number(value))
    }

    pub fn string(value: impl Into<JsString>) -> Self {
        Self::constant(Value::string(value.into()))
    }

    /// Construct a BigInt from its canonical decimal text.
    pub fn bigint(text: &str) -> JsResult<Self> {
        let value = JsBigInt::from_decimal(text).ok_or_else(|| {
            JsError::Fault(HostFault::new("invalid BigInt decimal text".to_owned()))
        })?;
        Ok(Self::constant(Value::bigint(value)))
    }

    fn constant(value: Value) -> Self {
        Self {
            value,
            origin: Origin::Constant,
        }
    }

    pub(crate) fn handle(value: Value, epoch: u32, call: u64) -> Self {
        Self {
            value,
            origin: Origin::Handle { epoch, call },
        }
    }

    pub(super) fn rooted_handle(
        value: Value,
        epoch: u32,
        call: u64,
        roots: &RefCell<HandleRoots>,
    ) -> Self {
        roots.borrow_mut().retain(&value);
        Self::handle(value, epoch, call)
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self.value.view(), ValueRef::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self.value.view(), ValueRef::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self.value.view(), ValueRef::Object(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(self.value.view(), ValueRef::Symbol(_))
    }

    pub fn is_bigint(&self) -> bool {
        matches!(self.value.view(), ValueRef::BigInt(_))
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value.view() {
            ValueRef::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.value.view() {
            ValueRef::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&JsString> {
        match self.value.view() {
            ValueRef::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bigint(&self) -> Option<&JsBigInt> {
        match self.value.view() {
            ValueRef::BigInt(value) => Some(value),
            _ => None,
        }
    }

    pub fn type_of(&self) -> &'static str {
        self.value.type_of()
    }

    pub fn truthy(&self) -> bool {
        self.value.truthy()
    }

    pub(crate) fn into_constant(self) -> Result<Value, HostFault> {
        match self.origin {
            Origin::Constant => Ok(self.value),
            Origin::Handle { .. } => Err(HostFault::new(
                "a JsValue handle cannot be used outside its call".to_owned(),
            )),
        }
    }
}

impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl fmt::Display for JsValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.value.view() {
            ValueRef::Undefined => formatter.write_str("undefined"),
            ValueRef::Null => formatter.write_str("null"),
            ValueRef::Bool(value) => write!(formatter, "{value}"),
            ValueRef::Number(value) => write!(formatter, "{value}"),
            ValueRef::String(value) => write!(formatter, "{value:?}"),
            ValueRef::BigInt(value) => write!(formatter, "{value}n"),
            ValueRef::Symbol(id) => write!(formatter, "symbol({id})"),
            ValueRef::Object(id) => write!(formatter, "object({id})"),
        }
    }
}

impl TraceRoots for JsValue {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.value.trace(tracer)
    }
}

#[derive(Default)]
pub(crate) struct HandleRoots {
    first: Option<Value>,
    rest: Vec<Value>,
}

impl HandleRoots {
    pub(super) fn retain(&mut self, value: &Value) {
        if !matches!(value.view(), ValueRef::Object(_) | ValueRef::Symbol(_)) {
            return;
        }
        match &self.first {
            None => self.first = Some(value.clone()),
            Some(first) if self.rest.last().unwrap_or(first) != value => {
                self.rest.push(value.clone())
            }
            Some(_) => {}
        }
    }
}

impl TraceRoots for HandleRoots {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.first.trace(tracer)?;
        self.rest.trace(tracer)
    }
}

impl TraceRoots for RefCell<HandleRoots> {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), crate::VmError> {
        self.borrow().trace(tracer)
    }
}
