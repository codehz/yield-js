//! Translate an explicit ECMAScript subset; never feed unchecked JS syntax to regex.

use std::borrow::Cow;
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegExpFlags {
    pub(crate) global: bool,
    pub(crate) ignore_case: bool,
    pub(crate) multiline: bool,
    pub(crate) dot_all: bool,
    pub(crate) sticky: bool,
    normalize_lines: bool,
}

impl RegExpFlags {
    pub(crate) fn as_string(self) -> String {
        [
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.sticky, 'y'),
        ]
        .into_iter()
        .filter_map(|(enabled, flag)| enabled.then_some(flag))
        .collect()
    }
}

/// Validate a literal using the same subset and engine limits as construction.
pub fn validate_regexp(source: &str, flags: &str) -> Result<(), String> {
    compile_pattern(source, flags).map(|_| ())
}

pub(crate) fn compile_pattern(
    source: &str,
    flags: &str,
) -> Result<(RegExpFlags, regex::Regex), String> {
    let mut seen = 0u8;
    for flag in flags.chars() {
        let bit = match flag {
            'g' => 1,
            'i' => 2,
            'm' => 4,
            's' => 8,
            'y' => 16,
            _ => return Err(format!("unsupported RegExp flag {flag:?}")),
        };
        if seen & bit != 0 {
            return Err(format!("duplicate RegExp flag {flag:?}"));
        }
        seen |= bit;
    }
    let mut flags = RegExpFlags {
        global: seen & 1 != 0,
        ignore_case: seen & 2 != 0,
        multiline: seen & 4 != 0,
        dot_all: seen & 8 != 0,
        sticky: seen & 16 != 0,
        normalize_lines: false,
    };
    let mut parser = Pattern {
        units: source.encode_utf16().collect(),
        pos: 0,
        out: String::new(),
        flags,
        has_anchor: false,
        explicit_line: false,
    };
    parser.disjunction(0)?;
    if parser.peek().is_some() {
        return Err("unmatched RegExp parenthesis".into());
    }
    flags.normalize_lines = flags.multiline && parser.has_anchor;
    if flags.normalize_lines && parser.explicit_line {
        return Err(
            "multiline RegExp anchors cannot be combined with explicit line terminators".into(),
        );
    }
    let regex = regex::RegexBuilder::new(&parser.out)
        .case_insensitive(flags.ignore_case)
        .multi_line(flags.normalize_lines)
        .build()
        .map_err(|error| format!("unsupported RegExp pattern: {error}"))?;
    Ok((flags, regex))
}

const LINES: [u16; 4] = [10, 13, 0x2028, 0x2029];
// ECMAScript WhiteSpace plus LineTerminator, not the Unicode White_Space property.
const SPACE: &str =
    r"\x09-\x0D\x20\xA0\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{F26FF}";

fn encode_unit(unit: u16, flags: RegExpFlags) -> char {
    match unit {
        // ECMAScript non-Unicode ignoreCase does not fold non-ASCII into ASCII.
        0x017f if flags.ignore_case => '\u{f2800}',
        0x212a if flags.ignore_case => '\u{f2801}',
        13 | 0x2028 | 0x2029 if flags.normalize_lines => '\n',
        // Preserve UTF-16 ordering for character class ranges and make each code
        // unit, including either half of an astral character, one engine scalar.
        0xd800..=0xffff => char::from_u32(0xf0000 + u32::from(unit - 0xd800)).unwrap(),
        _ => char::from_u32(u32::from(unit)).unwrap(),
    }
}

pub(crate) fn encode_input(input: &str, flags: RegExpFlags) -> Cow<'_, str> {
    if input
        .chars()
        .all(|c| (c as u32) < 0xd800 && encode_unit(c as u16, flags) == c)
    {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(
            input
                .encode_utf16()
                .map(|unit| encode_unit(unit, flags))
                .collect(),
        )
    }
}

struct Pattern {
    units: Vec<u16>,
    pos: usize,
    out: String,
    flags: RegExpFlags,
    has_anchor: bool,
    explicit_line: bool,
}

struct Atom {
    nullable: bool,
    captures: bool,
    nested_captures: bool,
    assertion: bool,
}

impl Atom {
    const CHAR: Self = Self {
        nullable: false,
        captures: false,
        nested_captures: false,
        assertion: false,
    };
    const ASSERTION: Self = Self {
        nullable: true,
        assertion: true,
        ..Self::CHAR
    };
}

impl Pattern {
    fn peek(&self) -> Option<u16> {
        self.units.get(self.pos).copied()
    }

    fn take(&mut self) -> Option<u16> {
        let unit = self.peek()?;
        self.pos += 1;
        Some(unit)
    }

    fn eat(&mut self, ascii: u8) -> bool {
        if self.peek() == Some(u16::from(ascii)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn disjunction(&mut self, depth: usize) -> Result<(bool, bool), String> {
        if depth > 64 {
            return Err("RegExp groups nested too deeply".into());
        }
        let mut captures = false;
        let mut alternative_nullable = true;
        let mut nullable = false;
        while let Some(unit) = self.take() {
            match unit {
                41 => {
                    self.pos -= 1;
                    break;
                } // )
                124 => {
                    // |
                    self.out.push('|');
                    nullable |= alternative_nullable;
                    alternative_nullable = true;
                }
                _ => {
                    let mut atom = self.atom(unit, depth)?;
                    self.quantifier(&mut atom)?;
                    alternative_nullable &= atom.nullable;
                    captures |= atom.captures;
                }
            }
        }
        Ok((nullable || alternative_nullable, captures))
    }

    fn atom(&mut self, unit: u16, depth: usize) -> Result<Atom, String> {
        match u8::try_from(unit).ok() {
            Some(b'(') => return self.group(depth),
            Some(b'[') => self.class()?,
            Some(b'.') => self.out.push_str(if self.flags.dot_all {
                "(?s:.)"
            } else {
                r"[^\n\r\u{2028}\u{2029}]"
            }),
            Some(b'^' | b'$') => {
                self.has_anchor = true;
                self.out.push(char::from(unit as u8));
                return Ok(Atom::ASSERTION);
            }
            Some(b'\\') => {
                if self.eat(b'b') {
                    self.out.push_str(r"(?-u:\b)");
                    return Ok(Atom::ASSERTION);
                }
                if self.eat(b'B') {
                    self.out.push_str(r"(?-u:\B)");
                    return Ok(Atom::ASSERTION);
                }
                if let Some(unit) = self.escape(false)? {
                    self.literal(unit)?;
                }
            }
            Some(b'*' | b'+' | b'?' | b'{' | b'}' | b']') => {
                return Err("unexpected RegExp metacharacter".into());
            }
            _ => self.literal(unit)?,
        }
        Ok(Atom::CHAR)
    }

    fn group(&mut self, depth: usize) -> Result<Atom, String> {
        let mut capture = true;
        self.out.push('(');
        if self.eat(b'?') {
            if self.eat(b':') {
                capture = false;
                self.out.push_str("?:");
            } else if self.eat(b'<') {
                self.out.push_str("?P<");
                let mut count = 0;
                while !self.eat(b'>') {
                    let ch = self
                        .take()
                        .and_then(|unit| u8::try_from(unit).ok())
                        .filter(|c| {
                            c.is_ascii_alphabetic()
                                || *c == b'_'
                                || (count > 0 && c.is_ascii_digit())
                        })
                        .ok_or("unsupported RegExp group name or lookbehind")?;
                    self.out.push(char::from(ch));
                    count += 1;
                }
                self.out.push('>');
            } else {
                return Err("unsupported RegExp group (lookaround or inline modifier)".into());
            }
        }
        let (nullable, nested_captures) = self.disjunction(depth + 1)?;
        if !self.eat(b')') {
            return Err("unclosed RegExp group".into());
        }
        self.out.push(')');
        Ok(Atom {
            nullable,
            captures: capture || nested_captures,
            nested_captures,
            assertion: false,
        })
    }

    fn quantifier(&mut self, atom: &mut Atom) -> Result<(), String> {
        let optional = if self.eat(b'*') {
            self.out.push('*');
            true
        } else if self.eat(b'+') {
            self.out.push('+');
            false
        } else if self.eat(b'?') {
            self.out.push('?');
            true
        } else if self.eat(b'{') {
            self.out.push('{');
            let optional = self.digits().ok_or("invalid RegExp quantifier")?;
            if self.eat(b',') {
                self.out.push(',');
                self.digits();
            }
            if !self.eat(b'}') {
                return Err("unclosed RegExp quantifier".into());
            }
            self.out.push('}');
            optional
        } else {
            return Ok(());
        };
        // Empty repetitions and captures nested in repeated groups have different
        // backtracking/capture-reset rules in ECMAScript and the Rust engine.
        if atom.assertion || atom.nullable || atom.nested_captures {
            return Err("unsupported RegExp quantifier on an assertion, nullable atom, or group containing captures".into());
        }
        atom.nullable = optional;
        if self.eat(b'?') {
            self.out.push('?');
        }
        Ok(())
    }

    fn digits(&mut self) -> Option<bool> {
        let start = self.pos;
        let mut zero = true;
        while let Some(ch @ 48..=57) = self.peek() {
            self.pos += 1;
            self.out.push(char::from(ch as u8));
            zero &= ch == 48;
        }
        (self.pos != start).then_some(zero)
    }

    fn literal(&mut self, unit: u16) -> Result<(), String> {
        if self.flags.ignore_case && unit > 127 {
            return Err(
                "non-ASCII RegExp literals and range endpoints with ignoreCase are unsupported"
                    .into(),
            );
        }
        self.explicit_line |= LINES.contains(&unit);
        write!(
            self.out,
            "\\u{{{:X}}}",
            encode_unit(unit, self.flags) as u32
        )
        .unwrap();
        Ok(())
    }

    fn class(&mut self) -> Result<(), String> {
        let negated = self.eat(b'^');
        if self.eat(b']') {
            self.out.push_str(if negated { "(?s:.)" } else { "[a&&b]" });
            return Ok(());
        }
        self.out.push_str(if negated { "[^" } else { "[" });
        while !self.eat(b']') {
            let start = self.class_atom()?;
            if self.peek() == Some(u16::from(b'-'))
                && self.units.get(self.pos + 1) != Some(&u16::from(b']'))
            {
                self.pos += 1;
                let start = start.ok_or("unsupported RegExp class range endpoint")?;
                self.literal(start)?;
                self.out.push('-');
                let end = self
                    .class_atom()?
                    .ok_or("unsupported RegExp class range endpoint")?;
                self.literal(end)?;
                self.explicit_line |= LINES.into_iter().any(|line| (start..=end).contains(&line));
            } else if let Some(unit) = start {
                self.literal(unit)?;
            }
        }
        self.out.push(']');
        Ok(())
    }

    fn class_atom(&mut self) -> Result<Option<u16>, String> {
        match self.take() {
            Some(92) => self.escape(true), // backslash
            Some(unit) => Ok(Some(unit)),
            None => Err("unclosed RegExp class".into()),
        }
    }

    fn escape(&mut self, in_class: bool) -> Result<Option<u16>, String> {
        let escape = self
            .take()
            .and_then(|unit| u8::try_from(unit).ok())
            .ok_or("invalid RegExp escape")?;
        let unit = match escape {
            b'd' | b'D' | b'w' | b'W' | b's' | b'S' => {
                self.out.push('[');
                if escape.is_ascii_uppercase() {
                    self.out.push('^');
                }
                self.out.push_str(match escape.to_ascii_lowercase() {
                    b'd' => "0-9",
                    b'w' => "A-Za-z0-9_",
                    _ => SPACE,
                });
                self.out.push(']');
                return Ok(None);
            }
            b'0' if !matches!(self.peek(), Some(48..=57)) => 0,
            b'b' if in_class => 8,
            b't' => 9,
            b'n' => 10,
            b'v' => 11,
            b'f' => 12,
            b'r' => 13,
            b'x' => self.hex(2)?,
            b'u' => self.hex(4)?,
            b'c' => u16::from(
                self.take()
                    .and_then(|unit| u8::try_from(unit).ok())
                    .filter(u8::is_ascii_alphabetic)
                    .ok_or("unsupported RegExp control escape")?
                    % 32,
            ),
            b'^' | b'$' | b'\\' | b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'{'
            | b'}' | b'|' | b'/' => u16::from(escape),
            b'-' if in_class => u16::from(escape),
            _ => {
                return Err(format!(
                    "unsupported RegExp escape \\{}",
                    char::from(escape)
                ));
            }
        };
        Ok(Some(unit))
    }

    fn hex(&mut self, len: usize) -> Result<u16, String> {
        let mut value = 0;
        for _ in 0..len {
            let digit = self
                .take()
                .and_then(|unit| char::from_u32(u32::from(unit)))
                .and_then(|ch| ch.to_digit(16))
                .ok_or("invalid RegExp hexadecimal escape")?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }
}
