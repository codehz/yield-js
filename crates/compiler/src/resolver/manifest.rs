//! Strict JSON reader for package manifests. Only resolver-relevant value kinds are retained.

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::str::Chars;

pub(super) type Map = BTreeMap<String, Value>;

pub(super) enum Value {
    Null,
    String(String),
    Object(Map),
    Other,
}

impl Value {
    pub(super) fn as_object(&self) -> Option<&Map> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
}

pub(super) fn parse(source: &str) -> Result<Map, &'static str> {
    let mut parser = Parser(
        source
            .strip_prefix('\u{feff}')
            .unwrap_or(source)
            .chars()
            .peekable(),
    );
    let value = parser.value(0)?;
    if parser.peek().is_some() {
        return Err("trailing data in package manifest");
    }
    match value {
        Value::Object(object) => Ok(object),
        _ => Err("package manifest must be a JSON object"),
    }
}

struct Parser<'a>(Peekable<Chars<'a>>);

impl Parser<'_> {
    fn peek(&mut self) -> Option<char> {
        while matches!(self.0.peek(), Some(' ' | '\t' | '\r' | '\n')) {
            self.0.next();
        }
        self.0.peek().copied()
    }

    fn take(&mut self, c: char) -> bool {
        self.0.next_if_eq(&c).is_some()
    }

    fn expect(&mut self, c: char) -> Result<(), &'static str> {
        if self.take(c) {
            Ok(())
        } else {
            Err("invalid package manifest JSON")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, &'static str> {
        if depth > 128 {
            return Err("package manifest exceeds maximum JSON depth");
        }
        match self.peek() {
            Some('{') => {
                self.0.next();
                let mut map = Map::new();
                if self.peek() != Some('}') {
                    loop {
                        self.peek();
                        let key = self.string()?;
                        self.peek();
                        self.expect(':')?;
                        map.insert(key, self.value(depth + 1)?);
                        if self.peek() != Some(',') {
                            break;
                        }
                        self.0.next();
                    }
                }
                self.expect('}')?;
                Ok(Value::Object(map))
            }
            Some('[') => {
                self.0.next();
                if self.peek() != Some(']') {
                    loop {
                        self.value(depth + 1)?;
                        if self.peek() != Some(',') {
                            break;
                        }
                        self.0.next();
                    }
                }
                self.expect(']')?;
                Ok(Value::Other)
            }
            Some('"') => self.string().map(Value::String),
            Some('n' | 't' | 'f') => {
                let first = self.0.next().expect("peeked literal");
                for c in match first {
                    'n' => "ull",
                    't' => "rue",
                    _ => "alse",
                }
                .chars()
                {
                    self.expect(c)?;
                }
                Ok(if first == 'n' {
                    Value::Null
                } else {
                    Value::Other
                })
            }
            Some('-' | '0'..='9') => {
                self.take('-');
                if !self.take('0') {
                    self.digits()?;
                }
                if self.take('.') {
                    self.digits()?;
                }
                if self.take('e') || self.take('E') {
                    if !self.take('+') {
                        self.take('-');
                    }
                    self.digits()?;
                }
                Ok(Value::Other)
            }
            _ => Err("invalid package manifest JSON value"),
        }
    }

    fn digits(&mut self) -> Result<(), &'static str> {
        if self.0.next_if(char::is_ascii_digit).is_none() {
            return Err("invalid number in package manifest JSON");
        }
        while self.0.next_if(char::is_ascii_digit).is_some() {}
        Ok(())
    }

    fn hex(&mut self) -> Result<u32, &'static str> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = self
                .0
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or("invalid Unicode escape in package manifest JSON")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, &'static str> {
        self.expect('"')?;
        let mut result = String::new();
        while let Some(c) = self.0.next() {
            let c = match c {
                '"' => return Ok(result),
                '\\' => match self.0.next() {
                    Some(c @ ('"' | '\\' | '/')) => c,
                    Some('b') => '\u{8}',
                    Some('f') => '\u{c}',
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('u') => {
                        let mut unit = self.hex()?;
                        if (0xd800..=0xdbff).contains(&unit) {
                            self.expect('\\')?;
                            self.expect('u')?;
                            let low = self.hex()?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err("unpaired surrogate in package manifest JSON");
                            }
                            unit = 0x10000 + (unit - 0xd800) * 0x400 + low - 0xdc00;
                        }
                        char::from_u32(unit).ok_or("unpaired surrogate in package manifest JSON")?
                    }
                    _ => return Err("invalid escape in package manifest JSON"),
                },
                '\0'..='\u{1f}' => {
                    return Err("unescaped control character in package manifest JSON");
                }
                _ => c,
            };
            result.push(c);
        }
        Err("unterminated string in package manifest JSON")
    }
}
