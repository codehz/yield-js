//! ECMAScript regexp flags and code-unit matching backed by the pinned `regress` parser.

use std::borrow::Cow;

use crate::value::JsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegExpFlags {
    pub(crate) global: bool,
    pub(crate) ignore_case: bool,
    pub(crate) multiline: bool,
    pub(crate) dot_all: bool,
    pub(crate) sticky: bool,
    pub(crate) unicode: bool,
    pub(crate) unicode_sets: bool,
    pub(crate) has_indices: bool,
}

impl RegExpFlags {
    pub(crate) fn as_string(self) -> String {
        [
            ('d', self.has_indices),
            ('g', self.global),
            ('i', self.ignore_case),
            ('m', self.multiline),
            ('s', self.dot_all),
            ('u', self.unicode),
            ('v', self.unicode_sets),
            ('y', self.sticky),
        ]
        .into_iter()
        .filter_map(|(flag, set)| set.then_some(flag))
        .collect()
    }
}

pub fn validate_regexp(source: &str, flags: &str) -> Result<(), String> {
    validate_regexp_string(&source.into(), flags)
}

pub(crate) fn validate_regexp_string(source: &JsString, flags: &str) -> Result<(), String> {
    compile_pattern(source, flags).map(|_| ())
}

pub(crate) fn compile_pattern(
    source: &JsString,
    flags: &str,
) -> Result<(RegExpFlags, regress::Regex), String> {
    let mut bits = 0u16;
    for flag in flags.chars() {
        let bit = match flag {
            'd' => 1 << 0,
            'g' => 1 << 1,
            'i' => 1 << 2,
            'm' => 1 << 3,
            's' => 1 << 4,
            'u' => 1 << 5,
            'v' => 1 << 6,
            'y' => 1 << 7,
            _ => return Err(format!("invalid RegExp flag {flag:?}")),
        };
        if bits & bit != 0 {
            return Err(format!("duplicate RegExp flag {flag:?}"));
        }
        bits |= bit;
    }
    if bits & (1 << 5) != 0 && bits & (1 << 6) != 0 {
        return Err("RegExp flags u and v are mutually exclusive".into());
    }
    let parsed = RegExpFlags {
        has_indices: bits & 1 != 0,
        global: bits & 2 != 0,
        ignore_case: bits & 4 != 0,
        multiline: bits & 8 != 0,
        dot_all: bits & 16 != 0,
        unicode: bits & 32 != 0,
        unicode_sets: bits & 64 != 0,
        sticky: bits & 128 != 0,
    };
    let backend_flags = format!(
        "{}{}{}{}{}",
        if parsed.ignore_case { "i" } else { "" },
        if parsed.multiline { "m" } else { "" },
        if parsed.dot_all { "s" } else { "" },
        if parsed.unicode { "u" } else { "" },
        if parsed.unicode_sets { "v" } else { "" }
    );
    let pattern = encode_input(source, parsed);
    let regex = regress::Regex::from_unicode(
        pattern.iter().map(|&unit| u32::from(unit)),
        backend_flags.as_str(),
    )
    .map_err(|error| format!("invalid RegExp pattern: {error}"))?;
    Ok((parsed, regex))
}

pub(crate) fn encode_input(input: &JsString, flags: RegExpFlags) -> Cow<'_, [u16]> {
    let mut units = input.to_utf16();
    if !flags.unicode && !flags.unicode_sets {
        legacy_casefold_units(&mut units, flags.ignore_case);
    }
    units
}
pub(crate) fn encode_input_with_flags<'a>(input: &'a JsString, flags: &JsString) -> Cow<'a, [u16]> {
    let mut units = input.to_utf16();
    let ignore_case = flags.units().any(|unit| unit == u16::from(b'i'));
    let unicode = flags.units().any(|unit| unit == u16::from(b'u'));
    let unicode_sets = flags.units().any(|unit| unit == u16::from(b'v'));
    if !unicode && !unicode_sets {
        legacy_casefold_units(&mut units, ignore_case);
    }
    units
}

// Keep legacy folds excluded by ECMAScript separate; map surrogate halves to scalars
// so the backend parser cannot pair them in non-Unicode mode.
fn legacy_casefold_units(units: &mut Cow<'_, [u16]>, ignore_case: bool) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit == 92
            && units.get(index + 1) == Some(&b'u'.into())
            && let Some(value) = escaped_special_unit(units, index, ignore_case)
        {
            for (digit, shift) in units.to_mut()[index + 2..index + 6]
                .iter_mut()
                .zip([12, 8, 4, 0])
            {
                *digit = u16::from(HEX[usize::from((value >> shift) & 0xf)]);
            }
            index += 6;
            continue;
        }
        let mapped = map_special_unit(unit, ignore_case);
        if mapped != unit {
            units.to_mut()[index] = mapped;
        }
        index += 1;
    }
}

fn escaped_special_unit(units: &[u16], index: usize, ignore_case: bool) -> Option<u16> {
    let hex = units.get(index + 2..index + 6)?;
    let value = hex.iter().try_fold(0u16, |value, unit| {
        Some(value * 16 + char::from_u32(u32::from(*unit))?.to_digit(16)? as u16)
    })?;
    ((ignore_case && matches!(value, 0x017f | 0x212a)) || (0xd800..=0xdfff).contains(&value))
        .then(|| map_special_unit(value, ignore_case))
}

fn map_special_unit(unit: u16, ignore_case: bool) -> u16 {
    match unit {
        0xd800..=0xdfff => 0xe800 + (unit - 0xd800),
        0x017f if ignore_case => 0xe000,
        0x212a if ignore_case => 0xe001,
        _ => unit,
    }
}
