//! Error object allocation, including suppressed and aggregate errors.

use std::fmt::Write;

use crate::error::{ErrorClass, VmError};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject};
use crate::host::{Host, PlatformServices};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
impl VmCore {
    /// Allocate an error of `class`, resolving built-in kinds through their
    /// constructor roots and declared host subclasses through the stored
    /// export prototypes. An undeclared host class is a host contract fault.
    pub(crate) fn alloc_class_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        class: ErrorClass,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        match class {
            ErrorClass::Builtin(kind) => self.alloc_error(
                roots,
                self.builtins.get(kind.builtin_root()),
                kind.name(),
                message,
            ),
            ErrorClass::Host { module, name } => {
                let prototype = self.error_export_prototype(module, name).ok_or_else(|| {
                    VmError::HostFault(format!("undeclared error class {module}:{name}"))
                })?;
                self.alloc_error(roots, prototype, name, message)
            }
        }
    }

    pub(crate) fn construct_error<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        name: &str,
        prototype: ObjectId,
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
                    roots, prototype, error, suppressed, message,
                )?
            } else {
                self.alloc_suppressed_error(roots, error, suppressed)?
            };
            return Ok(Invocation::Value(value));
        }
        let error = self.alloc_error(roots, prototype, name, message)?;
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
        let mut suffix = String::new();
        self.visit_stack_frames(roots, |name, module, line, column| {
            write!(suffix, "\n    at {name} ({module}:{line}:{column})")
                .expect("writing to String cannot fail");
        });
        stack.push_str(&JsString::from(suffix));
        self.heap
            .define_data(id, "stack", Value::string(stack), Attributes::METHOD)?;
        Ok(Value::object(id))
    }

    pub(crate) fn visit_stack_frames(
        &self,
        roots: &ActiveRoots<'_>,
        mut visit: impl FnMut(&str, &str, u32, u32),
    ) {
        // A frame can appear in more than one root view. Dedup by address,
        // not function ID: recursive calls are separate frames.
        let mut visible = [std::ptr::null(); 10];
        let mut depth = 0;
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
            visit(name, module, site.line, site.column);
            visible[depth] = address;
            depth += 1;
            if depth == visible.len() {
                break;
            }
        }
    }

    pub(crate) fn alloc_suppressed_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        error: Value,
        suppressed: Value,
    ) -> Result<Value, VmError> {
        let value = self.alloc_error(
            roots,
            self.builtins.get("suppressederror.prototype"),
            "SuppressedError",
            None,
        )?;
        let ValueRef::Object(id) = value.view() else {
            unreachable!()
        };
        self.define_suppressed_values(id, error, suppressed)?;
        Ok(value)
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
