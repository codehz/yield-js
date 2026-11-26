//! Synchronous exception reporting and VM-generated global notifications.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{EventPayload, EventState, Object};
use crate::host::{Diagnostics, Host, PlatformServices};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

impl VmCore {
    pub(crate) fn report_exception<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        error: Value,
    ) -> Result<(), VmError> {
        if self.reporting_exception {
            self.deliver_exception(host, error);
            return Ok(());
        }
        let message = self.exception_message(&error);
        let (filename, lineno, colno) = roots
            .frames()
            .find_map(|frame| {
                let function = &self.build.functions[frame.function];
                let module = function.module?;
                let pc = frame.pc.checked_sub(1)?;
                let sites = &function.stack_sites;
                let site = &sites[sites
                    .partition_point(|site| site.pc as usize <= pc)
                    .checked_sub(1)?];
                (site.line != 0).then(|| {
                    (
                        self.build.modules[module as usize].id.as_str().into(),
                        site.line,
                        site.column,
                    )
                })
            })
            .unwrap_or_default();
        let event = self.new_global_notification(
            host,
            "error",
            true,
            EventPayload::ErrorEvent {
                message,
                filename,
                lineno,
                colno,
                error: error.clone(),
            },
        )?;
        let roots = &roots.with(&error);
        self.reporting_exception = true;
        let result = self.dispatch_global_notification(host, roots, event);
        self.reporting_exception = false;
        if result? {
            self.deliver_exception(host, error);
        }
        Ok(())
    }

    pub(crate) fn new_global_notification<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        ty: &str,
        cancelable: bool,
        payload: EventPayload,
    ) -> Result<ObjectId, VmError> {
        let time_stamp = self.monotonic_millis(host);
        let value = self.alloc_event(
            None,
            EventState {
                ty: ty.into(),
                payload,
                cancelable,
                time_stamp,
                initialized: true,
                trusted: true,
                ..EventState::default()
            },
        )?;
        let ValueRef::Object(id) = value.view() else {
            unreachable!("event allocation")
        };
        Ok(id)
    }

    pub(crate) fn dispatch_global_notification<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        event: ObjectId,
    ) -> Result<bool, VmError> {
        match self.dispatch_event(host, roots, self.builtins.get("globalThis"), event, true)? {
            Invocation::Value(value) => Ok(value.truthy()),
            Invocation::Thrown(error) => Err(VmError::Thrown(error)),
        }
    }

    fn deliver_exception<P: PlatformServices>(&self, host: &Host<P>, error: Value) {
        let Some(sink) = host.platform().diagnostics() else {
            return;
        };
        let diagnostic = self.exception_diagnostic(&error);
        sink.report(crate::host::Diagnostic::Uncaught {
            error: self.host_value(error),
            diagnostic: &diagnostic,
        });
    }

    pub(crate) fn exception_diagnostic(&self, error: &Value) -> JsString {
        if let ValueRef::Object(id) = error.view()
            && matches!(self.heap.get(id), Ok(Object::Plain(object)) if object.is_error)
            && let Some(stack) = self.diagnostic_string(id, "stack")
        {
            return stack;
        }
        self.exception_message(error)
    }

    fn exception_message(&self, error: &Value) -> JsString {
        match error.view() {
            ValueRef::Object(id) if matches!(self.heap.get(id), Ok(Object::Plain(o)) if o.is_error) =>
            {
                let name = self
                    .diagnostic_string(id, "name")
                    .unwrap_or_else(|| "Error".into());
                let message = self.diagnostic_string(id, "message").unwrap_or_default();
                if name.is_empty() {
                    message
                } else if message.is_empty() {
                    name
                } else {
                    let mut name = name;
                    name.push_str(&": ".into());
                    name.push_str(&message);
                    name
                }
            }
            ValueRef::Object(_) => "Uncaught exception".into(),
            ValueRef::Symbol(id) => {
                let mut text: JsString = "Symbol(".into();
                if let Some(Some(description)) = self.heap.symbols.get(&id) {
                    text.push_str(description);
                }
                text.push_str(&")".into());
                text
            }
            _ => crate::ops::to_js_string(error),
        }
    }

    // Stop at accessors and proxies instead of running guest code to format a report.
    fn diagnostic_string(&self, mut id: ObjectId, key: &str) -> Option<JsString> {
        loop {
            let object = self.heap.get(id).ok()?;
            if matches!(object, Object::Proxy { .. }) {
                return None;
            }
            if let Some(property) = object.own_property(PropertyKeyRef::String(key)) {
                return match property.data_value()?.view() {
                    ValueRef::String(text) => Some(text.clone()),
                    _ => None,
                };
            }
            id = object.prototype()?;
        }
    }
}
