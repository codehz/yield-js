//! Incremental UTF-8 and UTF-16 codecs, independent of JS values and endpoints.

use std::str;

use crate::heap::TextEncoding;

/// One scalar from UTF-16 units: a surrogate pair, a lone surrogate as U+FFFD,
/// or one BMP code unit. The second value is the units consumed.
pub(crate) fn scalar_at(units: &[u16], index: usize) -> (char, usize) {
    let first = units[index];
    if (0xd800..=0xdbff).contains(&first)
        && let Some(&second) = units.get(index + 1)
        && (0xdc00..=0xdfff).contains(&second)
    {
        let code = 0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
        return (char::from_u32(code).expect("surrogate pair is a scalar"), 2);
    }
    if (0xd800..=0xdfff).contains(&first) {
        return ('\u{fffd}', 1);
    }
    (
        char::from_u32(u32::from(first)).expect("BMP unit is a scalar"),
        1,
    )
}

/// Incremental USVString conversion. An ending high surrogate is kept until
/// the next chunk determines whether it forms a pair or needs replacement.
#[derive(Default)]
pub(crate) struct Utf8Encoder {
    high: Option<u16>,
}

impl Utf8Encoder {
    pub(crate) fn encode(
        &mut self,
        units: impl ExactSizeIterator<Item = u16>,
        last: bool,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(units.len() * 3);
        let mut units = units.peekable();
        let mut encoded = [0u8; 4];
        let mut next = self.high.take().or_else(|| units.next());
        while let Some(first) = next {
            if !last && (0xd800..=0xdbff).contains(&first) && units.peek().is_none() {
                self.high = Some(first);
                break;
            }
            let pair = [first, units.peek().copied().unwrap_or_default()];
            let (scalar, consumed) = scalar_at(&pair, 0);
            out.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
            if consumed == 2 {
                units.next();
            }
            next = units.next();
        }
        out
    }
}

/// Decoder checkpoint between calls. Kept separate from the object header so
/// a caller can supply bytes and choose when to flush without invoking JS.
#[derive(Default)]
pub(crate) struct DecoderState {
    pub(crate) streaming: bool,
    pub(crate) bom_seen: bool,
    pub(crate) prefix: [u8; 3],
    pub(crate) prefix_len: u8,
    pub(crate) pending: Vec<u8>,
}

pub(crate) struct FatalDecode;

impl DecoderState {
    pub(crate) fn decode(
        &mut self,
        encoding: TextEncoding,
        fatal: bool,
        ignore_bom: bool,
        input: &[u8],
        stream: bool,
    ) -> Result<String, FatalDecode> {
        if !self.streaming {
            *self = Self::default();
        }
        self.streaming = stream;
        let mut decoder = Decoder {
            encoding,
            fatal,
            ignore_bom,
            bom_seen: self.bom_seen,
            prefix: self.prefix,
            prefix_len: usize::from(self.prefix_len),
            out: String::new(),
            retained: Vec::new(),
            pending: std::mem::take(&mut self.pending),
        };
        if decoder.feed(input, !stream) {
            self.bom_seen = decoder.bom_seen;
            self.prefix = decoder.prefix;
            self.prefix_len = decoder.prefix_len as u8;
            Ok(decoder.out)
        } else {
            // Fatal calls serialize no output, so they leave the BOM mark alone.
            self.prefix = [0; 3];
            self.prefix_len = 0;
            if stream {
                self.pending = decoder.retained;
            }
            Err(FatalDecode)
        }
    }
}

/// Working state for one feed.
struct Decoder {
    encoding: TextEncoding,
    fatal: bool,
    ignore_bom: bool,
    bom_seen: bool,
    prefix: [u8; 3],
    prefix_len: usize,
    out: String,
    /// Bytes after a consumed fatal error, kept only while the stream is open.
    retained: Vec<u8>,
    pending: Vec<u8>,
}

impl Decoder {
    fn push(&mut self, scalar: char) {
        if !self.ignore_bom && !self.bom_seen {
            self.bom_seen = true;
            if scalar == '\u{feff}' {
                return;
            }
        }
        self.out.push(scalar);
    }

    fn push_str(&mut self, text: &str) {
        for scalar in text.chars() {
            self.push(scalar);
        }
    }

    /// Feed the retained bytes followed by this call's input. Returns false
    /// when a fatal error stopped decoding.
    fn feed(&mut self, input: &[u8], last: bool) -> bool {
        let pending = std::mem::take(&mut self.pending);
        self.out.reserve(pending.len() + input.len());
        let cursor = Cursor {
            sources: [&pending, input],
            index: 0,
            at: 0,
        };
        match self.encoding {
            TextEncoding::Utf8 => self.feed_utf8(cursor, last),
            encoding => self.feed_utf16(cursor, last, encoding),
        }
    }

    fn feed_utf16(&mut self, mut cursor: Cursor<'_>, last: bool, encoding: TextEncoding) -> bool {
        let mut bytes = [0; 4];
        let mut len = self.prefix_len;
        bytes[..len].copy_from_slice(&self.prefix[..len]);
        loop {
            cursor.fill(&mut bytes, &mut len, 2);
            if len < 2 {
                break;
            }
            let first = encoding.utf16_unit([bytes[0], bytes[1]]);
            let (scalar, consumed) = if (0xd800..=0xdbff).contains(&first) {
                cursor.fill(&mut bytes, &mut len, 4);
                if len < 4 {
                    break;
                }
                let second = encoding.utf16_unit([bytes[2], bytes[3]]);
                if (0xdc00..=0xdfff).contains(&second) {
                    let (scalar, _) = scalar_at(&[first, second], 0);
                    (Some(scalar), 4)
                } else {
                    // The following unit is not part of the error. Reprocess it.
                    (None, 2)
                }
            } else {
                (char::from_u32(u32::from(first)), 2)
            };
            if let Some(scalar) = scalar {
                self.push(scalar);
            } else if self.fatal {
                if !last {
                    self.retained.extend_from_slice(&bytes[consumed..len]);
                    cursor.copy_remainder(&mut self.retained);
                }
                return false;
            } else {
                self.push('\u{fffd}');
            }
            bytes.copy_within(consumed..len, 0);
            len -= consumed;
        }
        if last {
            if len != 0 {
                if self.fatal {
                    return false;
                }
                // EOF with a pending high surrogate and/or a byte is one error.
                self.push('\u{fffd}');
            }
            self.prefix_len = 0;
        } else {
            self.prefix[..len].copy_from_slice(&bytes[..len]);
            self.prefix_len = len;
        }
        true
    }

    fn feed_utf8(&mut self, mut cursor: Cursor<'_>, last: bool) -> bool {
        let mut buffer = [0u8; 4];
        let mut len = self.prefix_len;
        buffer[..len].copy_from_slice(&self.prefix[..len]);
        loop {
            if len > 0 {
                while len < 4
                    && let Some(rest) = cursor.rest()
                {
                    let take = (4 - len).min(rest.len());
                    buffer[len..len + take].copy_from_slice(&rest[..take]);
                    cursor.take(take);
                    len += take;
                }
                // The buffer may already hold a complete scalar even when it
                // is shorter than four bytes, and four bytes may hold a valid
                // scalar followed by an incomplete tail.
                match str::from_utf8(&buffer[..len]) {
                    Ok(text) => {
                        self.push_str(text);
                        len = 0;
                        continue;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        if valid > 0 {
                            self.push_str(
                                str::from_utf8(&buffer[..valid]).expect("valid_up_to prefix"),
                            );
                        }
                        match error.error_len() {
                            Some(bad) => {
                                if self.fatal {
                                    if !last {
                                        let start = valid + bad;
                                        self.retained.extend_from_slice(&buffer[start..len]);
                                        cursor.copy_remainder(&mut self.retained);
                                    }
                                    self.prefix_len = 0;
                                    return false;
                                }
                                self.push('\u{fffd}');
                                let start = valid + bad;
                                buffer.copy_within(start..len, 0);
                                len -= start;
                                continue;
                            }
                            None => {
                                let tail = len - valid;
                                buffer.copy_within(valid..len, 0);
                                len = tail;
                                if cursor.rest().is_some() {
                                    continue;
                                }
                                if last {
                                    if self.fatal {
                                        self.prefix_len = 0;
                                        self.retained.clear();
                                        return false;
                                    }
                                    self.push('\u{fffd}');
                                    len = 0;
                                }
                                break;
                            }
                        }
                    }
                }
            }
            let Some(rest) = cursor.rest() else {
                break;
            };
            match str::from_utf8(rest) {
                Ok(text) => {
                    self.push_str(text);
                    cursor.take(text.len());
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        self.push_str(str::from_utf8(&rest[..valid]).expect("valid_up_to prefix"));
                        cursor.take(valid);
                    }
                    let rest = &rest[valid..];
                    match error.error_len() {
                        Some(bad) => {
                            if self.fatal {
                                if !last {
                                    self.retained.extend_from_slice(&rest[bad..]);
                                    cursor.skip_source();
                                    cursor.copy_remainder(&mut self.retained);
                                }
                                self.prefix_len = 0;
                                return false;
                            }
                            self.push('\u{fffd}');
                            cursor.take(bad);
                        }
                        None => {
                            // Keep the incomplete suffix; the next iteration
                            // completes it from the following source or flushes.
                            self.prefix[..rest.len()].copy_from_slice(rest);
                            self.prefix_len = rest.len();
                            buffer[..rest.len()].copy_from_slice(rest);
                            len = rest.len();
                            cursor.take(rest.len());
                        }
                    }
                }
            }
        }
        if last {
            self.prefix_len = 0;
        } else {
            self.prefix[..len].copy_from_slice(&buffer[..len]);
            self.prefix_len = len;
        }
        true
    }
}

struct Cursor<'a> {
    sources: [&'a [u8]; 2],
    index: usize,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn fill(&mut self, buffer: &mut [u8], len: &mut usize, target: usize) {
        while *len < target
            && let Some(rest) = self.rest()
        {
            let take = (target - *len).min(rest.len());
            buffer[*len..*len + take].copy_from_slice(&rest[..take]);
            self.take(take);
            *len += take;
        }
    }

    /// The unread remainder of the current source, skipping exhausted ones.
    fn rest(&mut self) -> Option<&'a [u8]> {
        while self.index < self.sources.len() && self.at == self.sources[self.index].len() {
            self.index += 1;
            self.at = 0;
        }
        (self.index < self.sources.len()).then(|| &self.sources[self.index][self.at..])
    }

    fn take(&mut self, count: usize) {
        self.at += count;
    }

    /// Consume the rest of the current source; the caller embeds it explicitly.
    fn skip_source(&mut self) {
        self.at = self.sources[self.index].len();
    }

    /// Append the unread remainder of the current source and every later one.
    fn copy_remainder(&self, out: &mut Vec<u8>) {
        if self.index < self.sources.len() {
            out.extend_from_slice(&self.sources[self.index][self.at..]);
            for source in &self.sources[self.index + 1..] {
                out.extend_from_slice(source);
            }
        }
    }
}
