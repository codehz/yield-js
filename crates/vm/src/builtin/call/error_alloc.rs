//! Error object allocation, including suppressed and aggregate errors.

use std::fmt::Write;

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{ClassArgs, ClassDef, ClassPrototype};
use crate::intrinsics::{AggregateError, Installed, MessageErrorClass, SuppressedError};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
impl VmCore {
    /// Allocate a built-in error using its intrinsic prototype.
    pub(crate) fn alloc_message_error<C: MessageErrorClass>(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        self.alloc_intrinsic_message(roots, C::CLASS, message)
    }

    pub(crate) fn alloc_intrinsic_message(
        &mut self,
        roots: &ActiveRoots<'_>,
        class: &ClassDef,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        let id = self.alloc_error_with(
            roots,
            self.builtins.get(class.prototype),
            class.key.owner,
            message,
        )?;
        Ok(Value::object(id))
    }

    pub(crate) fn initialize_error<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        prototype: ClassPrototype<'_>,
        name: &str,
        args: ClassArgs<'_>,
    ) -> Result<Invocation, VmError> {
        self.initialize_message_and_cause(
            host,
            roots,
            prototype.resolved(),
            name,
            args.get(0),
            args.get(1),
        )
    }

    fn initialize_message_and_cause<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        name: &str,
        message: Option<&Value>,
        options: Option<&Value>,
    ) -> Result<Invocation, VmError> {
        let message = match self.error_message(host, roots, message)? {
            Ok(message) => message,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let id = self.alloc_error_with(roots, prototype, name, message)?;
        if let Err(error) = self.install_error_cause(host, &roots.with(&id), id, options)? {
            return Ok(Invocation::Thrown(error));
        }
        Ok(Invocation::Value(Value::object(id)))
    }

    pub(crate) fn initialize_aggregate_error<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        prototype: ClassPrototype<'_>,
        name: &str,
        args: ClassArgs<'_>,
    ) -> Result<Invocation, VmError> {
        let value = match self.initialize_message_and_cause(
            host,
            roots,
            prototype.resolved(),
            name,
            args.get(1),
            args.get(2),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        let ValueRef::Object(id) = value.view() else {
            unreachable!()
        };
        let roots = &roots.with(&id);
        let mut errors = Vec::new();
        if let Some(iterable) = args
            .get(0)
            .filter(|arg| !matches!(arg.view(), ValueRef::Undefined))
        {
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
        self.define_aggregate_errors(id, errors)?;
        Ok(Invocation::Value(value))
    }

    pub(crate) fn initialize_suppressed_error<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        prototype: ClassPrototype<'_>,
        name: &str,
        args: ClassArgs<'_>,
    ) -> Result<Invocation, VmError> {
        let message = match self.error_message(host, roots, args.get(2))? {
            Ok(message) => message,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let id = self.alloc_error_with(roots, prototype.resolved(), name, message)?;
        let error = args.get(0).cloned().unwrap_or(Value::UNDEFINED);
        let suppressed = args.get(1).cloned().unwrap_or(Value::UNDEFINED);
        self.define_suppressed_values(id, error, suppressed)?;
        Ok(Invocation::Value(Value::object(id)))
    }

    fn error_message<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        message: Option<&Value>,
    ) -> Result<Result<Option<JsString>, Value>, VmError> {
        match message {
            None => Ok(Ok(None)),
            Some(value) if matches!(value.view(), ValueRef::Undefined) => Ok(Ok(None)),
            Some(value) => Ok(self.coerce_string(host, roots, value)?.map(Some)),
        }
    }

    fn install_error_cause<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        options: Option<&Value>,
    ) -> Result<Result<(), Value>, VmError> {
        if let Some(options) = options
            && let ValueRef::Object(options_id) = options.view()
        {
            let key = PropertyKeyRef::String("cause");
            match self.has_property_key(host, roots, options_id, key)? {
                Invocation::Value(present) if present.truthy() => {
                    let cause = match self.read_property(host, roots, options, key)? {
                        Invocation::Value(cause) => cause,
                        Invocation::Thrown(error) => return Ok(Err(error)),
                    };
                    self.heap
                        .define_data(id, "cause", cause, Attributes::METHOD)?;
                }
                Invocation::Value(_) => {}
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(()))
    }

    fn alloc_error_with(
        &mut self,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        name: &str,
        message: Option<JsString>,
    ) -> Result<ObjectId, VmError> {
        let id = self
            .heap
            .alloc(Object::Plain(PlainObject::error(prototype)));
        self.write_error_properties(roots, id, name, message)?;
        Ok(id)
    }

    fn write_error_properties(
        &mut self,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        name: &str,
        message: Option<JsString>,
    ) -> Result<(), VmError> {
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
        Ok(())
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
        let class = SuppressedError::CLASS;
        let id = self.alloc_error_with(
            roots,
            self.builtins.get(class.prototype),
            class.key.owner,
            None,
        )?;
        self.define_suppressed_values(id, error, suppressed)?;
        Ok(Value::object(id))
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
        let class = AggregateError::CLASS;
        let id = self.alloc_error_with(
            roots,
            self.builtins.get(class.prototype),
            class.key.owner,
            Some("All promises were rejected".into()),
        )?;
        self.define_aggregate_errors(id, errors)?;
        Ok(Value::object(id))
    }

    fn define_aggregate_errors(&mut self, id: ObjectId, errors: Vec<Value>) -> Result<(), VmError> {
        let errors = self.heap.alloc_array(errors);
        self.heap
            .define_data(id, "errors", Value::object(errors), Attributes::METHOD)?;
        Ok(())
    }
}
