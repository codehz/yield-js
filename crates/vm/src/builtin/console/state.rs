//! Commit Console state in algorithm order; Printer failures do not roll it back.

use std::time::Duration;

use crate::console::{ConsoleEvent, ConsoleGroup, ConsoleLevel, ConsolePayload};
use crate::gc::ActiveRoots;
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::ValueRef;
use crate::vm::VmCore;
use crate::{JsString, Value, VmError, ops};

use super::{Method, logging};

pub(super) fn call(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    method: Method,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let result = match method {
        Method::Clear => {
            vm.console.groups.clear();
            print(vm, host, ConsoleLevel::Clear, ConsolePayload::Clear)
        }
        Method::GroupEnd => {
            vm.console.groups.pop();
            Ok(())
        }
        Method::Group | Method::GroupCollapsed => {
            let default = Value::string("group".into());
            let args = if args.is_empty() {
                std::slice::from_ref(&default)
            } else {
                args
            };
            let title = match logging::format(vm, host, roots, args)? {
                Ok(title) => title,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let collapsed = matches!(method, Method::GroupCollapsed);
            let level = if collapsed {
                ConsoleLevel::GroupCollapsed
            } else {
                ConsoleLevel::Group
            };
            print(vm, host, level, ConsolePayload::Args(title.clone())).map(|()| {
                vm.console.groups.push(ConsoleGroup { title, collapsed });
            })
        }
        Method::Count | Method::CountReset | Method::Time | Method::TimeLog | Method::TimeEnd => {
            let label = match args
                .first()
                .filter(|value| !matches!(value.view(), ValueRef::Undefined))
            {
                Some(value) => match vm.coerce_string(host, roots, value)? {
                    Ok(label) => label,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
                None => "default".into(),
            };
            label_method(vm, host, args.get(1..).unwrap_or_default(), method, label)
        }
        Method::Assert
        | Method::Debug
        | Method::Dir
        | Method::Dirxml
        | Method::Error
        | Method::Info
        | Method::Log
        | Method::Table
        | Method::Trace
        | Method::Warn => match logging::payload(vm, host, roots, method, args)? {
            Ok(Some((level, payload))) => print(vm, host, level, payload),
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        },
    };
    Ok(match result {
        Ok(()) => Invocation::Value(Value::UNDEFINED),
        Err(error) => Invocation::Thrown(error),
    })
}

fn label_method(
    vm: &mut VmCore,
    host: &Host,
    rest: &[Value],
    method: Method,
    mut label: JsString,
) -> Result<(), Value> {
    let (level, message, rest) = match method {
        Method::Count => {
            let number = vm.console.counts.entry(label.clone()).or_default();
            *number += 1.0;
            label.push_str(&": ".into());
            label.push_str(&ops::to_js_string(&Value::number(*number)));
            (ConsoleLevel::Count, label, &[][..])
        }
        Method::CountReset => {
            if let Some(count) = vm.console.counts.get_mut(&label) {
                *count = 0.0;
                return Ok(());
            }
            (
                ConsoleLevel::CountReset,
                prefixed("No count for label: ", &label),
                &[][..],
            )
        }
        Method::Time => {
            vm.console
                .timers
                .entry(label)
                .or_insert_with(|| vm.clock.elapsed());
            return Ok(());
        }
        Method::TimeLog | Method::TimeEnd => {
            let end = matches!(method, Method::TimeEnd);
            let start = if end {
                vm.console.timers.remove(&label)
            } else {
                vm.console.timers.get(&label).copied()
            };
            let Some(start) = start else {
                return print_message(
                    vm,
                    host,
                    ConsoleLevel::Warn,
                    prefixed("No timer for label: ", &label),
                    &[],
                );
            };
            let elapsed = vm.clock.elapsed().saturating_sub(start);
            label.push_str(&": ".into());
            label.push_str(&milliseconds(elapsed).into());
            if end {
                (ConsoleLevel::TimeEnd, label, &[][..])
            } else {
                (ConsoleLevel::TimeLog, label, rest)
            }
        }
        _ => unreachable!("not a console label method"),
    };
    print_message(vm, host, level, message, rest)
}

fn print_message(
    vm: &mut VmCore,
    host: &Host,
    level: ConsoleLevel,
    message: JsString,
    rest: &[Value],
) -> Result<(), Value> {
    let mut args = Vec::with_capacity(1 + rest.len());
    args.push(crate::console::ConsoleArg::Value(
        crate::console::ConsolePreview {
            mode: crate::console::ConsolePreviewMode::Optimal,
            value: crate::console::ConsoleValue::String(message),
        },
    ));
    args.extend(rest.iter().map(|value| {
        crate::console::ConsoleArg::Value(logging::preview(
            vm,
            value,
            crate::console::ConsolePreviewMode::Optimal,
        ))
    }));
    print(vm, host, level, ConsolePayload::Args(args))
}

fn print(
    vm: &mut VmCore,
    host: &Host,
    level: ConsoleLevel,
    payload: ConsolePayload,
) -> Result<(), Value> {
    let Some(sink) = host.builtin_services().console() else {
        return Err(vm.alloc_dom_exception(
            "NotSupportedError".into(),
            "Console service is not available".into(),
        ));
    };
    sink.write(&ConsoleEvent {
        level,
        groups: vm.console.groups.clone(),
        payload,
    });
    Ok(())
}

fn prefixed(prefix: &str, label: &JsString) -> JsString {
    let mut text = JsString::from(prefix);
    text.push_str(label);
    text
}

fn milliseconds(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    let fraction = elapsed.subsec_nanos() % 1_000_000;
    if fraction == 0 {
        format!("{millis} ms")
    } else {
        let fraction = format!("{fraction:06}");
        format!("{millis}.{} ms", fraction.trim_end_matches('0'))
    }
}
