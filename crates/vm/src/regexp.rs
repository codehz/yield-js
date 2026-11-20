//! ECMAScript regexp flags and code-unit matching backed by the pinned `regress` parser.

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
    compile_pattern(&source.into(), flags).map(|_| ())
}

pub(crate) fn validate_regexp_units(source: &[u16], flags: &str) -> Result<(), String> {
    compile_pattern(&source.into(), flags).map(|_| ())
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
    let pattern = if parsed.unicode || parsed.unicode_sets {
        source.as_units().to_vec()
    } else {
        legacy_casefold_units(source.as_units(), parsed.ignore_case)
    };
    let regex = regress::Regex::from_unicode(
        pattern.iter().map(|&unit| u32::from(unit)),
        backend_flags.as_str(),
    )
    .map_err(|error| format!("invalid RegExp pattern: {error}"))?;
    Ok((parsed, regex))
}

pub(crate) fn encode_input(input: &[u16], flags: RegExpFlags) -> Vec<u16> {
    if flags.unicode || flags.unicode_sets {
        input.to_vec()
    } else {
        legacy_casefold_units(input, flags.ignore_case)
    }
}

// Keep legacy folds excluded by ECMAScript separate; map surrogate halves to scalars
// so the backend parser cannot pair them in non-Unicode mode.
fn legacy_casefold_units(units: &[u16], ignore_case: bool) -> Vec<u16> {
    let mut mapped = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit == 92
            && units.get(index + 1) == Some(&b'u'.into())
            && let Some(value) = escaped_special_unit(units, index, ignore_case)
        {
            mapped.extend(format!("\\u{value:04X}").encode_utf16());
            index += 6;
            continue;
        }
        mapped.push(map_special_unit(unit, ignore_case));
        index += 1;
    }
    mapped
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
