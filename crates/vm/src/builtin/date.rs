//! Date stores a clipped UTC instant; the active host supplies local time and now.

mod prototype;
mod text;

use crate::builtin::date_calendar::{local_to_utc, make_date, make_day, make_time, time_clip};
use crate::builtin::globals::Owner;
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, link_constructor,
};
use crate::error::VmError;
use crate::exec::Hint;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject, TO_PRIMITIVE_SYMBOL};
use crate::host::{Host, TimeService};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

#[yield_binding_macros::builtin_global(name = "Date", root = "date", kind = "constructor")]
mod members {
    use super::{Invocation, Value, numeric_time};

    static_calls! {
        now: 0 => |vm, _, _, _, _| Ok(Invocation::Value(Value::number(vm.time().now_ms() as f64)));
        parse: 1 => |vm, host, roots, args, _| {
            let roots = &roots.with(&args);
            let time = match vm.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(string) => super::text::parse(&string, vm.time())?,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            Ok(numeric_time(time))
        };
        UTC: 7 => |vm, host, roots, args, _| {
            let roots = &roots.with(&args);
            Ok(match super::calendar_arguments(vm, host, roots, &args)? {
                Ok(time) => numeric_time(super::time_clip(time)),
                Err(error) => Invocation::Thrown(error),
            })
        };
    }

    prototype_calls! {
        getDate: 0 | getDay: 0 | getFullYear: 0 | getHours: 0 | getMilliseconds: 0
        | getMinutes: 0 | getMonth: 0 | getSeconds: 0 | getTime: 0 | getTimezoneOffset: 0
        | getUTCDate: 0 | getUTCDay: 0 | getUTCFullYear: 0 | getUTCHours: 0
        | getUTCMilliseconds: 0 | getUTCMinutes: 0 | getUTCMonth: 0 | getUTCSeconds: 0
        | setDate: 1 | setFullYear: 3 | setHours: 4 | setMilliseconds: 1 | setMinutes: 3
        | setMonth: 2 | setSeconds: 2 | setTime: 1 | setUTCDate: 1 | setUTCFullYear: 3
        | setUTCHours: 4 | setUTCMilliseconds: 1 | setUTCMinutes: 3 | setUTCMonth: 2
        | setUTCSeconds: 2 | getYear: 0 | setYear: 1 | toDateString: 0 | toISOString: 0
        | toJSON: 1 | toLocaleDateString: 0 | toLocaleString: 0 | toLocaleTimeString: 0
        | toString: 0 | toTimeString: 0 | toUTCString: 0 | valueOf: 0 as name =>
            |vm, host, roots, args, receiver| super::prototype::method(vm, host, roots, name, receiver, &args);
    }
}

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(members::NAME, Slot::Global, "");
const TO_PRIMITIVE: IntrinsicKey =
    IntrinsicKey::new(members::NAME, Slot::PrototypeSymbol, "toPrimitive");
const PROTOTYPE: &str = "date.prototype";
pub(crate) const OWNER: Owner = Owner::new(
    members::ROOT,
    &[
        RootSpec::new(
            members::ROOT,
            RootKind::Native(CONSTRUCTOR),
            FUNCTION_PARENT,
        )
        .fixed(PROTOTYPE),
        RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[
        &[IntrinsicDef::constructor_with_target(
            CONSTRUCTOR,
            7,
            |vm, _, _, _, _| {
                let clock = vm.time();
                let now = clock.now_ms();
                let text = text::format_local(now, clock, text::LocalPart::Full)?;
                Ok(Invocation::Value(Value::string(text.into())))
            },
            construct_date,
        )],
        members::INTRINSICS,
        &[IntrinsicDef::call(
            TO_PRIMITIVE,
            1,
            |vm, host, roots, args, receiver| {
                prototype::to_primitive(
                    vm,
                    host,
                    roots,
                    receiver,
                    args.first().unwrap_or(&Value::UNDEFINED),
                )
            },
        )],
    ],
    Some(install),
);

fn install(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(members::ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    members::install_members(vm, constructor, prototype);
    let utc_string = vm
        .heap
        .get(prototype)
        .expect("Date prototype")
        .own_property(PropertyKeyRef::String("toUTCString"))
        .and_then(|property| property.data_value().cloned())
        .expect("Date toUTCString method");
    define_builtin_data(vm, prototype, "toGMTString", utc_string, Attributes::METHOD);
    let to_primitive = vm.alloc_intrinsic(TO_PRIMITIVE);
    define_builtin_data(
        vm,
        prototype,
        crate::value::PropertyKey::Symbol(TO_PRIMITIVE_SYMBOL),
        Value::object(to_primitive),
        Attributes::FUNCTION_METADATA,
    );
}

fn construct_date(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let time = match args.as_slice() {
        [] => Some(vm.time().now_ms()),
        [arg] => {
            if let ValueRef::Object(id) = arg.view()
                && let Object::Date { time, .. } = vm.heap.get(id)?
            {
                *time
            } else {
                let primitive = match vm.coerce_primitive(host, roots, arg, Hint::Default)? {
                    Ok(primitive) => primitive,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if let ValueRef::String(string) = primitive.view() {
                    text::parse(string, vm.time())?
                } else {
                    match vm.coerce_number(host, roots, &primitive)? {
                        Ok(number) => time_clip(number),
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
            }
        }
        _ => match calendar_arguments(vm, host, roots, &args)? {
            Ok(time) => clip_local(time, vm.time())?,
            Err(error) => return Ok(Invocation::Thrown(error)),
        },
    };
    let prototype =
        match vm.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => id,
                _ => vm.builtins.get(PROTOTYPE),
            },
            thrown => return Ok(thrown),
        };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::Date {
            object: PlainObject::new(Some(prototype)),
            time,
        },
    ))))
}

fn calendar_arguments(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Result<Result<f64, Value>, VmError> {
    let mut numbers = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (number, value) in numbers.iter_mut().zip(args) {
        match vm.coerce_number(host, roots, value)? {
            Ok(value) => *number = value,
            Err(error) => return Ok(Err(error)),
        }
    }
    let [year, month, day, hour, minute, second, millisecond] = numbers;
    Ok(Ok(make_date(
        make_day(full_year(year), month, day),
        make_time(hour, minute, second, millisecond),
    )))
}

fn full_year(year: f64) -> f64 {
    let integer = year.trunc();
    if (0.0..=99.0).contains(&integer) {
        1900.0 + integer
    } else {
        year
    }
}

fn clip_local(time: f64, clock: &dyn TimeService) -> Result<Option<i64>, VmError> {
    if !time.is_finite() {
        return Ok(None);
    }
    Ok(time_clip(local_to_utc(time, &clock.time_zone())?))
}

fn numeric_time(time: Option<i64>) -> Invocation {
    Invocation::Value(Value::number(time.map_or(f64::NAN, |time| time as f64)))
}
