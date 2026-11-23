use std::collections::VecDeque;

use crate::console::{
    ConsoleArg, ConsoleFragment, ConsoleFrame, ConsoleLevel, ConsolePayload, ConsolePreview,
    ConsolePreviewMode::{self, Generic, Optimal},
    ConsoleValue,
    preview::Preview,
};
use crate::gc::ActiveRoots;
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::ValueRef;
use crate::{JsString, Value, Vm, VmError};

use super::Method;

pub(super) type Completion<T> = Result<Result<T, Value>, VmError>;

macro_rules! convert {
    ($value:expr) => {
        match $value? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

pub(super) fn payload(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    method: Method,
    args: &[Value],
) -> Completion<Option<(ConsoleLevel, ConsolePayload)>> {
    let (level, args) = match method {
        Method::Debug => (ConsoleLevel::Debug, args),
        Method::Error => (ConsoleLevel::Error, args),
        Method::Info => (ConsoleLevel::Info, args),
        Method::Log => (ConsoleLevel::Log, args),
        Method::Warn => (ConsoleLevel::Warn, args),
        Method::Dirxml => (ConsoleLevel::Dirxml, args),
        Method::Assert => {
            let data = match args.split_first() {
                Some((condition, _)) if condition.truthy() => return Ok(Ok(None)),
                Some((_, data)) => data,
                None => &[],
            };
            let mut message = JsString::from("Assertion failed");
            let rest = if let Some((first, rest)) = data.split_first()
                && let ValueRef::String(first) = first.view()
            {
                message.push_str(&": ".into());
                message.push_str(first);
                rest
            } else {
                data
            };
            let data: Vec<_> = std::iter::once(Value::string(message))
                .chain(rest.iter().cloned())
                .collect();
            let args = convert!(format(vm, host, roots, &data));
            return Ok(Ok(Some((ConsoleLevel::Assert, ConsolePayload::Args(args)))));
        }
        Method::Trace => {
            let mut frames = Vec::new();
            vm.visit_stack_frames(roots, |name, module, line, column| {
                frames.push(ConsoleFrame {
                    name: name.into(),
                    module: module.into(),
                    line,
                    column,
                });
            });
            let args = convert!(format(vm, host, roots, args));
            return Ok(Ok(Some((
                ConsoleLevel::Trace,
                ConsolePayload::Trace { args, frames },
            ))));
        }
        Method::Dir => {
            let options = match args.get(1) {
                None => None,
                Some(value) => match value.view() {
                    ValueRef::Undefined => None,
                    ValueRef::Object(_) | ValueRef::Null => Some(preview(vm, value, Generic)),
                    _ => {
                        return Ok(Err(vm.type_error(
                            roots,
                            "console.dir options must be an object or null",
                        )?));
                    }
                },
            };
            let value = preview(vm, args.first().unwrap_or(&Value::UNDEFINED), Generic);
            return Ok(Ok(Some((
                ConsoleLevel::Dir,
                ConsolePayload::Dir { value, options },
            ))));
        }
        Method::Table => {
            let undefined = Value::UNDEFINED;
            let value = args.first().unwrap_or(&undefined);
            let table = convert!(super::table::build(vm, host, roots, value, args.get(1)));
            return Ok(Ok(Some((
                ConsoleLevel::Log,
                table.unwrap_or_else(|| ConsolePayload::Args(raw(vm, std::slice::from_ref(value)))),
            ))));
        }
        _ => unreachable!("not a console logging method"),
    };
    if args.is_empty() {
        return Ok(Ok(None));
    }
    let args = convert!(format(vm, host, roots, args));
    Ok(Ok(Some((level, ConsolePayload::Args(args)))))
}

pub(super) fn preview(vm: &Vm, value: &Value, mode: ConsolePreviewMode) -> ConsolePreview {
    ConsolePreview {
        mode,
        value: Preview::new(vm).value(value, 0),
    }
}

pub(super) fn raw(vm: &Vm, args: &[Value]) -> Vec<ConsoleArg> {
    args.iter()
        .map(|value| ConsoleArg::Value(preview(vm, value, Optimal)))
        .collect()
}

pub(super) fn format(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Completion<Vec<ConsoleArg>> {
    let [first, rest @ ..] = args else {
        return Ok(Ok(Vec::new()));
    };
    let ValueRef::String(target) = first.view() else {
        return Ok(Ok(raw(vm, args)));
    };
    if rest.is_empty() || !target.units().any(|unit| unit == u16::from(b'%')) {
        return Ok(Ok(raw(vm, args)));
    }
    let mut source: VecDeque<_> = target.units().collect();
    let mut fragments = Vec::new();
    let mut text = Vec::new();
    let mut rest = rest;
    let mut formatted = false;
    while !rest.is_empty()
        && let Some(unit) = source.pop_front()
    {
        let specifier = if unit == u16::from(b'%') {
            source.front().and_then(|&unit| u8::try_from(unit).ok())
        } else {
            None
        };
        let Some(specifier @ (b's' | b'd' | b'i' | b'f' | b'c' | b'o' | b'O')) = specifier else {
            text.push(unit);
            continue;
        };
        source.pop_front();
        let (value, remaining) = rest.split_first().expect("one format argument");
        rest = remaining;
        formatted = true;
        if specifier == b's' {
            let invocation = if matches!(value.view(), ValueRef::Symbol(_)) {
                vm.symbol_method(roots, value, "toString")?
            } else {
                vm.string_value(host, roots, value)?
            };
            let string = match invocation {
                Invocation::Value(value) => value.into_string().expect("String result"),
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            // Formatter recurs on the replaced target, including inserted text.
            if rest.is_empty() {
                text.extend(string.units());
            } else {
                for unit in string.units().collect::<Vec<_>>().into_iter().rev() {
                    source.push_front(unit);
                }
                if text.last() == Some(&u16::from(b'%')) {
                    source.push_front(text.pop().expect("percent prefix"));
                }
            }
            continue;
        }
        if !text.is_empty() {
            fragments.push(ConsoleFragment::Text(JsString::from_units(std::mem::take(
                &mut text,
            ))));
        }
        fragments.push(match specifier {
            b'c' => ConsoleFragment::Style(convert!(vm.coerce_string(host, roots, value))),
            b'o' => ConsoleFragment::Value(preview(vm, value, Optimal)),
            b'O' => ConsoleFragment::Value(preview(vm, value, Generic)),
            _ => {
                let number = if matches!(value.view(), ValueRef::Symbol(_)) {
                    f64::NAN
                } else {
                    let radix = Value::number(10.0);
                    let result = if specifier == b'f' {
                        vm.call_number_static(
                            host,
                            roots,
                            "parseFloat",
                            std::slice::from_ref(value),
                        )?
                    } else {
                        vm.call_number_static(host, roots, "parseInt", &[value.clone(), radix])?
                    };
                    match result {
                        Invocation::Value(value) => match value.view() {
                            ValueRef::Number(number) => number,
                            _ => unreachable!("numeric parser result"),
                        },
                        Invocation::Thrown(error) => return Ok(Err(error)),
                    }
                };
                ConsoleFragment::Value(ConsolePreview {
                    mode: Optimal,
                    value: ConsoleValue::Number(number),
                })
            }
        });
    }
    if !formatted {
        return Ok(Ok(raw(vm, args)));
    }
    text.extend(source);
    if !text.is_empty() {
        fragments.push(ConsoleFragment::Text(JsString::from_units(text)));
    }
    let mut result = Vec::with_capacity(1 + rest.len());
    result.push(ConsoleArg::Formatted(fragments));
    result.extend(
        rest.iter()
            .map(|value| ConsoleArg::Value(preview(vm, value, Optimal))),
    );
    Ok(Ok(result))
}
