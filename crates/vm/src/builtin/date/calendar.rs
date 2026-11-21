//! Proleptic Gregorian arithmetic and embedded IANA offset lookup for Date.

use chrono::{DateTime, LocalResult, Offset, TimeZone};
use chrono_tz::{GapInfo, Tz};
use jiff::Timestamp;
use jiff::tz::{AmbiguousOffset, TimeZone as JiffZone};

use crate::VmError;

pub(crate) const TIME_CLIP_BOUND: i64 = 8_640_000_000_000_000;
const DAY_MS: i64 = 86_400_000;
const UNIX_EPOCH_DAY: i64 = 719_468; // Days since March 1 of year zero.
const CYCLE_DAYS: i64 = 146_097;

pub(crate) fn make_day(year: f64, month: f64, day: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !day.is_finite() {
        return f64::NAN;
    }
    let year = year.trunc();
    let month = month.trunc();
    let day = day.trunc();
    // With opposing signs, cancel complete years from the month before
    // division so large but cancelling inputs can still select a small year.
    let (year, month) = if year.is_sign_negative() != month.is_sign_negative()
        && (month / 12.0 + year).abs() < year.abs() / 2.0
    {
        (0.0, year.mul_add(12.0, month))
    } else {
        (year, month)
    };
    let month_in_year = month.rem_euclid(12.0);
    let year = year + ((month - month_in_year) / 12.0).round();
    // Shift the start of the year to March so February is at the end.
    let march_year = year - if month_in_year < 2.0 { 1.0 } else { 0.0 };
    let era = (march_year / 400.0).floor();
    let year_in_era = march_year.rem_euclid(400.0);
    let march_month = (month_in_year + 10.0).rem_euclid(12.0);
    let day_in_era = 365.0 * year_in_era + (year_in_era / 4.0).floor()
        - (year_in_era / 100.0).floor()
        + ((153.0 * march_month + 2.0) / 5.0).floor();
    era.mul_add(CYCLE_DAYS as f64, day) + (day_in_era - UNIX_EPOCH_DAY as f64 - 1.0)
}

pub(crate) fn make_time(hour: f64, minute: f64, second: f64, millisecond: f64) -> f64 {
    if !hour.is_finite() || !minute.is_finite() || !second.is_finite() || !millisecond.is_finite() {
        return f64::NAN;
    }
    hour.trunc() * 3_600_000.0
        + minute.trunc() * 60_000.0
        + second.trunc() * 1_000.0
        + millisecond.trunc()
}

pub(crate) fn make_date(day: f64, time: f64) -> f64 {
    let result = day * DAY_MS as f64 + time;
    if result.is_finite() { result } else { f64::NAN }
}

pub(crate) fn time_clip(time: f64) -> Option<i64> {
    (time.abs() <= TIME_CLIP_BOUND as f64).then_some(time as i64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fields {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub weekday: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub millisecond: i64,
}

pub(crate) fn fields(time: i64) -> Fields {
    let day = time.div_euclid(DAY_MS);
    let time = time.rem_euclid(DAY_MS);
    let march_day = day + UNIX_EPOCH_DAY;
    let era = march_day.div_euclid(CYCLE_DAYS);
    let day_in_era = march_day.rem_euclid(CYCLE_DAYS);
    let year_in_era =
        (day_in_era - day_in_era / 1460 + day_in_era / 36524 - day_in_era / 146096) / 365;
    let day_in_year = day_in_era - (365 * year_in_era + year_in_era / 4 - year_in_era / 100);
    let march_month = (5 * day_in_year + 2) / 153;
    let month = (march_month + 2) % 12;
    Fields {
        year: era * 400 + year_in_era + i64::from(month < 2),
        month,
        day: day_in_year - (153 * march_month + 2) / 5 + 1,
        weekday: (day + 4).rem_euclid(7),
        hour: time / 3_600_000,
        minute: time / 60_000 % 60,
        second: time / 1_000 % 60,
        millisecond: time % 1_000,
    }
}

pub(crate) fn offset_ms(time: i64, zone: &str) -> Result<i64, VmError> {
    Ok(i64::from(zone_offset(time, parse_zone(zone)?, false)?) * 1000)
}

/// Extract wall-clock fields from a clipped UTC instant. The wall clock
/// itself can fall outside the TimeClip range.
pub(crate) fn local_fields(time: i64, zone: &str) -> Result<Fields, VmError> {
    Ok(fields(time + offset_ms(time, zone)?))
}

/// Convert a wall-clock time (encoded as though UTC) without clipping either
/// side of the conversion: a zone offset can move it inside the Date range.
pub(crate) fn local_to_utc(time: f64, zone: &str) -> Result<f64, VmError> {
    let zone = parse_zone(zone)?;
    if !time.is_finite() {
        return Ok(f64::NAN);
    }
    if time.abs() >= i64::MAX as f64 {
        // No zone adjustment can make this representable by Date.
        return Ok(time);
    }
    Ok(time - f64::from(zone_offset(time.floor() as i64, zone, true)?) * 1000.0)
}

fn parse_zone(zone: &str) -> Result<Tz, VmError> {
    zone.parse()
        .map_err(|_| VmError::HostFault(format!("unknown IANA time zone: {zone}")))
}

fn zone_offset(time: i64, zone: Tz, local: bool) -> Result<i32, VmError> {
    // chrono-tz's explicit transitions stop in 2099. Do not extrapolate its
    // last offset into the future; use the matching TZif recurrence instead.
    const START_2100: i64 = 4_102_444_800_000;
    if time >= START_2100 {
        return future_offset(time, fields(time).year, zone, local);
    }
    // Year zero predates every embedded transition. Use that first fixed
    // span for ancient wall clocks and instants outside chrono's calendar.
    const START_YEAR_ZERO: i64 = -62_167_219_200_000;
    let time = time.max(START_YEAR_ZERO);
    let dt = DateTime::from_timestamp_millis(time)
        .expect("limited years fit in chrono")
        .naive_utc();
    if !local {
        return Ok(zone.offset_from_utc_datetime(&dt).fix().local_minus_utc());
    }
    let offset = match zone.from_local_datetime(&dt) {
        LocalResult::Single(date) => *date.offset(),
        LocalResult::Ambiguous(a, b) => *a.min(b).offset(),
        LocalResult::None => GapInfo::new(&dt, &zone)
            .and_then(|gap| gap.begin)
            .map(|(_, offset)| offset)
            .ok_or_else(|| VmError::HostFault(format!("missing pre-gap offset for {zone}")))?,
    };
    Ok(offset.fix().local_minus_utc())
}

fn future_offset(time: i64, year: i64, zone: Tz, local: bool) -> Result<i32, VmError> {
    let tz = future_zone(zone)?;
    let time = if year >= 2800 {
        // Whole Gregorian cycles preserve the wall time and weekday. Never
        // expose the surrogate year to callers; only use its zone offset.
        let delta_days = i128::from(year.div_euclid(400) - 6) * i128::from(CYCLE_DAYS);
        (i128::from(time) - delta_days * i128::from(DAY_MS)) as i64
    } else {
        time
    };
    let ts = Timestamp::from_millisecond(time).expect("surrogate year fits in jiff");
    let offset = if local {
        let dt = jiff::tz::Offset::UTC.to_datetime(ts);
        match tz.to_ambiguous_timestamp(dt).offset() {
            AmbiguousOffset::Unambiguous { offset } => offset,
            AmbiguousOffset::Gap { before, .. } | AmbiguousOffset::Fold { before, .. } => before,
        }
    } else {
        tz.to_offset(ts)
    };
    Ok(offset.seconds())
}

fn future_zone(zone: Tz) -> Result<JiffZone, VmError> {
    // Read just the POSIX footer. Parsing historical TZif transitions here
    // would allocate a table that this path never uses.
    let rule = jiff_tzdb::get(zone.name())
        .and_then(|(_, data)| data.strip_suffix(b"\n"))
        .and_then(|data| data.rsplit(|&byte| byte == b'\n').next())
        .and_then(|rule| std::str::from_utf8(rule).ok())
        .ok_or_else(|| VmError::HostFault(format!("missing embedded future rule for {zone}")))?;
    JiffZone::posix(rule).map_err(|error| {
        VmError::HostFault(format!("invalid embedded future rule for {zone}: {error}"))
    })
}
