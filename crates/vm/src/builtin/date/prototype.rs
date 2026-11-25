use super::{clip_local, full_year, numeric_time, text};
use crate::builtin::date_calendar::{
    fields, local_fields, make_date, make_day, make_time, offset_ms, time_clip,
};
use crate::error::VmError;
use crate::exec::Hint;
use crate::gc::ActiveRoots;
use crate::heap::Object;
use crate::host::{Host, TimeService};
use crate::runtime::{Arguments, Invocation};
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: f64 = 60_000.0;

pub(super) fn method(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    key: &str,
    receiver: &Value,
    args: &[Value],
) -> Result<Invocation, VmError> {
    if key == "toJSON" {
        return to_json(vm, host, roots, receiver);
    }
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, "Date method requires a Date receiver");
    };
    let Object::Date { time, .. } = vm.heap.get(id)? else {
        return vm.throw_type_error(roots, "Date method requires a Date receiver");
    };
    let time = *time;
    if let Some(setter) = Setter::from_key(key) {
        let roots = &roots.with(receiver);
        let rooted_args = args;
        let roots = &roots.with(&rooted_args);
        let updated = match setter.calculate(vm, host, roots, time, args)? {
            Ok(Update::Store(updated)) => updated,
            Ok(Update::KeepInvalid) => return Ok(numeric_time(None)),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Object::Date { time, .. } = vm.heap.get_mut(id)? else {
            unreachable!("rooted Date receiver retains its internal slot")
        };
        *time = updated;
        return Ok(numeric_time(updated));
    }

    let local_part = match key {
        "toString" | "toLocaleString" => Some(text::LocalPart::Full),
        "toDateString" | "toLocaleDateString" => Some(text::LocalPart::Date),
        "toTimeString" | "toLocaleTimeString" => Some(text::LocalPart::Time),
        _ => None,
    };
    if let Some(part) = local_part {
        return Ok(match time {
            Some(time) => string(text::format_local(time, vm.time(), part)?),
            None => string("Invalid Date"),
        });
    }
    match key {
        "getTime" | "valueOf" => return Ok(numeric_time(time)),
        "toISOString" => {
            return match time {
                Some(time) => Ok(string(text::format_iso(time))),
                None => vm.throw_range_error(roots, "Invalid time value"),
            };
        }
        "toUTCString" => {
            return Ok(match time {
                Some(time) => string(text::format_utc(time)),
                None => string("Invalid Date"),
            });
        }
        _ => {}
    }
    let Some(time) = time else {
        return Ok(numeric_time(None));
    };
    if key == "getTimezoneOffset" {
        let offset = offset_ms(time, &vm.time().time_zone())?;
        return Ok(Invocation::Value(Value::number(
            (-offset) as f64 / MINUTE_MS,
        )));
    }
    let utc_field = key.strip_prefix("getUTC");
    let f = if utc_field.is_some() {
        fields(time)
    } else {
        local_fields(time, &vm.time().time_zone())?
    };
    let value = match utc_field.unwrap_or_else(|| key.strip_prefix("get").unwrap_or(key)) {
        "FullYear" => f.year,
        "Year" => f.year - 1900,
        "Month" => f.month,
        "Date" => f.day,
        "Day" => f.weekday,
        "Hours" => f.hour,
        "Minutes" => f.minute,
        "Seconds" => f.second,
        "Milliseconds" => f.millisecond,
        _ => unreachable!("unknown Date prototype method: {key}"),
    };
    Ok(numeric_time(Some(value)))
}

fn string(value: impl Into<crate::JsString>) -> Invocation {
    Invocation::Value(Value::string(value.into()))
}

fn to_json(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
        return vm.throw_type_error(
            roots,
            "Date.prototype.toJSON requires an object-coercible receiver",
        );
    }
    let object = Value::object(vm.coercible_object(receiver));
    let roots = &roots.with(&object);
    match vm.coerce_primitive(host, roots, &object, Hint::Number)? {
        Ok(value) => {
            if matches!(value.view(), ValueRef::Number(number) if !number.is_finite()) {
                return Ok(Invocation::Value(Value::NULL));
            }
        }
        Err(error) => return Ok(Invocation::Thrown(error)),
    }
    match vm.read_property(host, roots, &object, PropertyKeyRef::String("toISOString"))? {
        Invocation::Value(method) => vm.invoke(host, roots, &method, Arguments::EMPTY, &object),
        thrown @ Invocation::Thrown(_) => Ok(thrown),
    }
}

pub(super) fn to_primitive(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    hint: &Value,
) -> Result<Invocation, VmError> {
    if !matches!(receiver.view(), ValueRef::Object(_)) {
        return vm.throw_type_error(
            roots,
            "Date.prototype[Symbol.toPrimitive] requires an object receiver",
        );
    }
    let hint = match hint.view() {
        ValueRef::String(hint) if hint == "string" || hint == "default" => Hint::String,
        ValueRef::String(hint) if hint == "number" => Hint::Number,
        _ => {
            return vm
                .throw_type_error(roots, "Invalid hint for Date.prototype[Symbol.toPrimitive]");
        }
    };
    vm.ordinary_to_primitive(host, roots, receiver, hint)
}

#[derive(Clone, Copy)]
enum Component {
    Time,
    Year,
    LegacyYear,
    Month,
    Date,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
}

struct Setter {
    component: Component,
    utc: bool,
}

enum Update {
    Store(Option<i64>),
    KeepInvalid,
}

impl Setter {
    fn from_key(key: &str) -> Option<Self> {
        let name = key.strip_prefix("set")?;
        let (component, utc) = match name {
            "Time" => (Component::Time, true),
            "Year" => (Component::LegacyYear, false),
            name => {
                let utc = name.strip_prefix("UTC");
                let component = match utc.unwrap_or(name) {
                    "FullYear" => Component::Year,
                    "Month" => Component::Month,
                    "Date" => Component::Date,
                    "Hours" => Component::Hours,
                    "Minutes" => Component::Minutes,
                    "Seconds" => Component::Seconds,
                    "Milliseconds" => Component::Milliseconds,
                    _ => return None,
                };
                (component, utc.is_some())
            }
        };
        Some(Self { component, utc })
    }

    fn calculate(
        self,
        vm: &mut VmCore,
        host: &Host,
        roots: &ActiveRoots<'_>,
        initial: Option<i64>,
        args: &[Value],
    ) -> Result<Result<Update, Value>, VmError> {
        use Component::*;
        let count = match self.component {
            Hours => 4,
            Year | Minutes => 3,
            Month | Seconds => 2,
            _ => 1,
        };
        let wall_time = |clock: &dyn TimeService| -> Result<Option<i64>, VmError> {
            match initial {
                Some(time) if !self.utc => Ok(Some(time + offset_ms(time, &clock.time_zone())?)),
                time => Ok(time),
            }
        };
        // Invalid year setters start at wall-clock midnight, not LocalTime(0).
        // Annex B establishes the base before coercing the legacy year.
        let mut year_base = if matches!(self.component, LegacyYear) {
            Some(wall_time(vm.time())?.unwrap_or(0))
        } else {
            None
        };
        let first =
            match vm.coerce_number(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
        if matches!(self.component, Time) {
            return Ok(Ok(Update::Store(time_clip(first))));
        }
        // Full-year setters establish the base after the year but before
        // optional coercions.
        if matches!(self.component, Year) {
            year_base = Some(wall_time(vm.time())?.unwrap_or(0));
        }
        // Omitted optional arguments use the base time's fields; explicit
        // undefined is converted to NaN just like any other supplied value.
        let mut numbers = [None; 4];
        for (number, value) in numbers[1..count].iter_mut().zip(args.iter().skip(1)) {
            match vm.coerce_number(host, roots, value)? {
                Ok(value) => *number = Some(value),
                Err(error) => return Ok(Err(error)),
            }
        }
        let Some(wall_time) = (match year_base {
            Some(base) => Some(base),
            None => wall_time(vm.time())?,
        }) else {
            // A coercion may already have mutated the receiver. Do not write
            // NaN back when the algorithm returns early on the captured slot.
            return Ok(Ok(Update::KeepInvalid));
        };
        let f = fields(wall_time);
        let arg = |index: usize, default: i64| numbers[index].unwrap_or(default as f64);
        let year = f.year as f64;
        let month = f.month as f64;
        let date = f.day as f64;
        let hour = f.hour as f64;
        let minute = f.minute as f64;
        let second = f.second as f64;
        let day_number = || wall_time.div_euclid(DAY_MS) as f64;
        let time_within_day = || wall_time.rem_euclid(DAY_MS) as f64;
        let (day, time) = match self.component {
            Year => (
                make_day(first, arg(1, f.month), arg(2, f.day)),
                time_within_day(),
            ),
            LegacyYear => (make_day(full_year(first), month, date), time_within_day()),
            Month => (make_day(year, first, arg(1, f.day)), time_within_day()),
            Date => (make_day(year, month, first), time_within_day()),
            Hours => (
                day_number(),
                make_time(
                    first,
                    arg(1, f.minute),
                    arg(2, f.second),
                    arg(3, f.millisecond),
                ),
            ),
            Minutes => (
                day_number(),
                make_time(hour, first, arg(1, f.second), arg(2, f.millisecond)),
            ),
            Seconds => (
                day_number(),
                make_time(hour, minute, first, arg(1, f.millisecond)),
            ),
            Milliseconds => (day_number(), make_time(hour, minute, second, first)),
            Time => unreachable!(),
        };
        let time = make_date(day, time);
        let clipped = if self.utc {
            time_clip(time)
        } else {
            clip_local(time, vm.time())?
        };
        Ok(Ok(Update::Store(clipped)))
    }
}
