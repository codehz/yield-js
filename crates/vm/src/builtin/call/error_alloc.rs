//! Error object allocation, including suppressed and aggregate errors.

use std::fmt::Write;

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
impl Vm {
    pub(crate) fn construct_error(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        name: &str,
        prototype: &'static str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let message_arg = match name {
            "AggregateError" => args.get(1),
            "SuppressedError" => args.get(2),
            _ => args.first(),
        };
        let message = match message_arg.unwrap_or(&Value::UNDEFINED) {
            value if matches!(value.view(), ValueRef::Undefined) => None,
            value => match self.coerce_string(host, roots, value)? {
                Ok(message) => Some(message),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        if name == "SuppressedError" {
            let error = args.first().cloned().unwrap_or(Value::UNDEFINED);
            let suppressed = args.get(1).cloned().unwrap_or(Value::UNDEFINED);
            let value = if message.is_some() {
                self.alloc_suppressed_error_with_message(
                    roots,
                    self.builtins.get(prototype),
                    error,
                    suppressed,
                    message,
                )?
            } else {
                self.alloc_suppressed_error(error, suppressed)?
            };
            return Ok(Invocation::Value(value));
        }
        let error = self.alloc_error(roots, self.builtins.get(prototype), name, message)?;
        let ValueRef::Object(id) = error.view() else {
            unreachable!()
        };
        let roots = &roots.with(&error);
        let options = args.get(if name == "AggregateError" { 2 } else { 1 });
        if let Some(options) = options
            && let ValueRef::Object(options_id) = options.view()
        {
            let key = PropertyKeyRef::String("cause");
            match self.has_property_key(host, roots, options_id, key)? {
                Invocation::Value(present) if present.truthy() => {
                    let cause = match self.read_property(host, roots, options, key)? {
                        Invocation::Value(cause) => cause,
                        Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
                    };
                    self.heap
                        .define_data(id, "cause", cause, Attributes::METHOD)?;
                }
                Invocation::Value(_) => {}
                Invocation::Thrown(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        if name == "AggregateError" {
            let undefined = Value::UNDEFINED;
            let iterable = args.first().unwrap_or(&undefined);
            let mut errors = Vec::new();
            if !matches!(iterable.view(), ValueRef::Undefined) {
                let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
                    Ok(iterator) => iterator,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                loop {
                    match self.iterator_step(
                        host,
                        &roots.with(&(&iterator, &next, &errors)),
                        &iterator,
                        &next,
                    )? {
                        Ok(Some(value)) => errors.push(value),
                        Ok(None) => break,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
            }
            let errors = self.heap.alloc_array(errors);
            self.heap
                .define_data(id, "errors", Value::object(errors), Attributes::METHOD)?;
        }
        Ok(Invocation::Value(error))
    }

    pub(crate) fn alloc_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        name: &str,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        let mut object = PlainObject::new(Some(prototype));
        object.is_error = true;
        let id = self.heap.alloc(Object::Plain(object));
        let mut stack = JsString::from(format!("{name}: "));
        if let Some(message) = message {
            stack.push_str(&message);
            self.heap
                .define_data(id, "message", Value::string(message), Attributes::METHOD)?;
        }
        // A frame can appear in more than one root view. Dedup by address,
        // not function ID: recursive calls are separate frames.
        let mut visible = [std::ptr::null(); 10];
        let mut depth = 0;
        let mut suffix = String::new();
        for frame in roots.frames() {
            let address = std::ptr::from_ref(frame);
            if visible[..depth].contains(&address) {
                continue;
            }
            let Some(pc) = frame.pc.checked_sub(1) else {
                continue;
            };
            let function = &self.build.functions[frame.function];
            let Some(module) = function.module else {
                continue;
            };
            let sites = &function.stack_sites;
            let Some(index) = sites
                .partition_point(|site| site.pc as usize <= pc)
                .checked_sub(1)
            else {
                continue;
            };
            let site = &sites[index];
            if site.line == 0 {
                continue;
            }
            let name = if function.initial_name.is_empty() {
                "<anonymous>"
            } else {
                &function.initial_name
            };
            let module = &self.build.modules[module as usize].id;
            write!(
                suffix,
                "\n    at {name} ({module}:{}:{})",
                site.line, site.column
            )
            .expect("writing to String cannot fail");
            visible[depth] = address;
            depth += 1;
            if depth == visible.len() {
                break;
            }
        }
        if !suffix.is_empty() {
            stack.push_str(&JsString::from(suffix));
        }
        self.heap
            .define_data(id, "stack", Value::string(stack), Attributes::METHOD)?;
        Ok(Value::object(id))
    }

    pub(crate) fn alloc_suppressed_error(
        &mut self,
        error: Value,
        suppressed: Value,
    ) -> Result<Value, VmError> {
        let id = self
            .heap
            .alloc_plain_with(Some(self.builtins.get("suppressederror.prototype")));
        self.define_suppressed_values(id, error, suppressed)?;
        Ok(Value::object(id))
    }

    fn alloc_suppressed_error_with_message(
        &mut self,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        error: Value,
        suppressed: Value,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        let value = self.alloc_error(roots, prototype, "SuppressedError", message)?;
        let ValueRef::Object(id) = value.view() else {
            unreachable!()
        };
        self.define_suppressed_values(id, error, suppressed)?;
        Ok(value)
    }

    fn define_suppressed_values(
        &mut self,
        id: ObjectId,
        error: Value,
        suppressed: Value,
    ) -> Result<(), VmError> {
        self.heap
            .define_data(id, "error", error, Attributes::METHOD)?;
        self.heap
            .define_data(id, "suppressed", suppressed, Attributes::METHOD)?;
        Ok(())
    }

    pub(crate) fn alloc_aggregate_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        errors: Vec<Value>,
    ) -> Result<Value, VmError> {
        let error = self.alloc_error(
            roots,
            self.builtins.get("aggregateerror.prototype"),
            "AggregateError",
            Some("All promises were rejected".into()),
        )?;
        let ValueRef::Object(id) = error.view() else {
            unreachable!()
        };
        let errors = self.heap.alloc_array(errors);
        self.heap
            .define_data(id, "errors", Value::object(errors), Attributes::METHOD)?;
        Ok(error)
    }
}
