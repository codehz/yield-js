use std::fmt::Write;

use crate::builtin::date_calendar::{
    Fields, fields, local_to_utc, make_date, make_day, make_time, offset_ms, time_clip,
};
use crate::host::TimeService;
use crate::{JsString, VmError};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MINUTE_MS: i64 = 60_000;

pub(super) enum LocalPart {
    Full,
    Date,
    Time,
}

pub(super) fn parse(text: &JsString, clock: &impl TimeService) -> Result<Option<i64>, VmError> {
    // All supported spellings are ASCII; borrow the Latin-1 storage directly.
    let Some(text) = text
        .latin1_bytes()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return Ok(None);
    };

    if let Some((wall, offset)) = parse_iso(text) {
        let utc = match offset {
            Some(offset) => wall - offset as f64,
            None => local_to_utc(wall, &clock.time_zone())?,
        };
        Ok(time_clip(utc))
    } else if let Some((wall, offset)) = parse_canonical(text) {
        let Some(printed) = offset else {
            return Ok(time_clip(wall));
        };
        let minute_instant = wall as i64 - printed;
        // A local TimeString prints only whole offset minutes. Recover a
        // matching subminute offset from the active IANA zone, if there is one.
        // Probe both ends because the rounded instant can straddle a transition.
        let zone = clock.time_zone();
        let negative = printed < 0 || text.ends_with("GMT-0000");
        for nearby in [
            minute_instant,
            minute_instant - 59_000,
            minute_instant + 59_000,
        ] {
            let actual = offset_ms(nearby, &zone)?;
            if (actual < 0) == negative && actual / MINUTE_MS * MINUTE_MS == printed {
                let exact = wall as i64 - actual;
                if (nearby == exact || actual == offset_ms(exact, &zone)?)
                    && let Some(instant) = time_clip(exact as f64)
                {
                    return Ok(Some(instant));
                }
            }
        }
        Ok(time_clip(minute_instant as f64))
    } else {
        Ok(None)
    }
}

// An absent offset means a local date-time, not a date-only form.
fn parse_iso(mut text: &str) -> Option<(f64, Option<i64>)> {
    let sign = take_sign(&mut text);
    let year = number(&mut text, if sign.is_some() { 6 } else { 4 })?;
    if sign == Some(-1) && year == 0 {
        return None;
    }
    let year = year * sign.unwrap_or(1);
    let (mut month, mut day) = (1, 1);
    if take(&mut text, "-") {
        month = number(&mut text, 2)?;
        if take(&mut text, "-") {
            day = number(&mut text, 2)?;
        }
    }
    let day = calendar_day(year, month, day)?;
    if text.is_empty() {
        return Some((make_date(day, 0.0), Some(0)));
    }
    if !take(&mut text, "T") {
        return None;
    }
    let time = clock(&mut text, false)?;
    let offset = if text.is_empty() {
        None
    } else if take(&mut text, "Z") {
        Some(0)
    } else {
        Some(parse_offset(&mut text, true)?)
    };
    text.is_empty().then(|| (make_date(day, time), offset))
}

fn parse_canonical(text: &str) -> Option<(f64, Option<i64>)> {
    let mut tokens = text.split(' ');
    let weekday = tokens.next()?;
    let utc = weekday.ends_with(',');
    let weekday = weekday.strip_suffix(',').unwrap_or(weekday);
    let first = tokens.next()?;
    let second = tokens.next()?;
    let (month, mut day) = if utc {
        (second, first)
    } else {
        (first, second)
    };
    let month = MONTHS.iter().position(|&name| name == month)? as i64 + 1;
    if day.len() != 2 {
        return None;
    }
    let day = number(&mut day, 2)?;
    let mut year = tokens.next()?;
    let negative = take(&mut year, "-");
    let width = year.len();
    if !(4..=6).contains(&width) || (width > 4 && year.starts_with('0')) {
        return None;
    }
    let year = number(&mut year, width)?;
    if negative && year == 0 {
        return None;
    }
    let day = calendar_day(if negative { -year } else { year }, month, day)?;
    let midnight = make_date(day, 0.0);
    if WEEKDAYS[fields(midnight as i64).weekday as usize] != weekday {
        return None;
    }

    let mut time = tokens.next()?;
    let millis = clock(&mut time, true)?;
    let mut zone = tokens.next()?;
    if !time.is_empty() || tokens.next().is_some() || !take(&mut zone, "GMT") {
        return None;
    }
    let offset = if utc {
        None
    } else {
        Some(parse_offset(&mut zone, false)?)
    };
    zone.is_empty().then(|| (make_date(day, millis), offset))
}

fn calendar_day(year: i64, month: i64, day: i64) -> Option<f64> {
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)),
        _ => return None,
    };
    (1..=days)
        .contains(&day)
        .then(|| make_day(year as f64, (month - 1) as f64, day as f64))
}

fn clock(text: &mut &str, canonical: bool) -> Option<f64> {
    let hour = number(text, 2)?;
    if !take(text, ":") {
        return None;
    }
    let minute = number(text, 2)?;
    let (mut second, mut millisecond) = (0, 0);
    if take(text, ":") {
        second = number(text, 2)?;
        if !canonical && take(text, ".") {
            millisecond = number(text, 3)?;
        }
    } else if canonical {
        return None;
    }
    if minute > 59
        || second > 59
        || hour > 24
        || (hour == 24 && (canonical || minute != 0 || second != 0 || millisecond != 0))
    {
        return None;
    }
    Some(make_time(
        hour as f64,
        minute as f64,
        second as f64,
        millisecond as f64,
    ))
}

fn parse_offset(text: &mut &str, colon: bool) -> Option<i64> {
    let sign = take_sign(text)?;
    let hour = number(text, 2)?;
    if colon && !take(text, ":") {
        return None;
    }
    let minute = number(text, 2)?;
    (hour < 24 && minute < 60).then_some(sign * (hour * 60 + minute) * MINUTE_MS)
}

fn take_sign(text: &mut &str) -> Option<i64> {
    if take(text, "+") {
        Some(1)
    } else if take(text, "-") {
        Some(-1)
    } else {
        None
    }
}

fn take(text: &mut &str, prefix: &str) -> bool {
    if let Some(rest) = text.strip_prefix(prefix) {
        *text = rest;
        true
    } else {
        false
    }
}

fn number(text: &mut &str, width: usize) -> Option<i64> {
    let digits = text.get(..width)?;
    let mut value = 0;
    for digit in digits.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(digit - b'0');
    }
    *text = &text[width..];
    Some(value)
}

pub(super) fn format_local(
    time: i64,
    clock: &impl TimeService,
    part: LocalPart,
) -> Result<String, VmError> {
    let offset = offset_ms(time, &clock.time_zone())?;
    let f = fields(time + offset);
    let mut text = String::with_capacity(match part {
        LocalPart::Full => 37,
        LocalPart::Date => 18,
        LocalPart::Time => 17,
    });
    if !matches!(part, LocalPart::Time) {
        write!(text, "{} {} {:02} ", weekday(&f), month(&f), f.day).unwrap();
        write_year(&mut text, f.year, false);
    }
    if matches!(part, LocalPart::Full) {
        text.push(' ');
    }
    if !matches!(part, LocalPart::Date) {
        write_clock(&mut text, &f);
        let minutes = offset.abs() / MINUTE_MS;
        let sign = if offset < 0 { '-' } else { '+' };
        write!(text, " GMT{sign}{:02}{:02}", minutes / 60, minutes % 60).unwrap();
    }
    Ok(text)
}

pub(super) fn format_iso(time: i64) -> String {
    let f = fields(time);
    let mut text = String::with_capacity(27);
    write_year(&mut text, f.year, true);
    write!(text, "-{:02}-{:02}T", f.month + 1, f.day).unwrap();
    write_clock(&mut text, &f);
    write!(text, ".{:03}Z", f.millisecond).unwrap();
    text
}

pub(super) fn format_utc(time: i64) -> String {
    let f = fields(time);
    let mut text = String::with_capacity(32);
    write!(text, "{}, {:02} {} ", weekday(&f), f.day, month(&f)).unwrap();
    write_year(&mut text, f.year, false);
    text.push(' ');
    write_clock(&mut text, &f);
    text.push_str(" GMT");
    text
}

fn weekday(f: &Fields) -> &'static str {
    WEEKDAYS[f.weekday as usize]
}

fn month(f: &Fields) -> &'static str {
    MONTHS[f.month as usize]
}

fn write_year(text: &mut String, year: i64, iso: bool) {
    if iso && !(0..=9999).contains(&year) {
        let sign = if year < 0 { '-' } else { '+' };
        write!(text, "{sign}{:06}", year.abs()).unwrap();
    } else {
        if year < 0 {
            text.push('-');
        }
        write!(text, "{:04}", year.abs()).unwrap();
    }
}

fn write_clock(text: &mut String, f: &Fields) {
    write!(text, "{:02}:{:02}:{:02}", f.hour, f.minute, f.second).unwrap();
}
