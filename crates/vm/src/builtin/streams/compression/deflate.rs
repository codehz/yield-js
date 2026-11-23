use super::Format;
use miniz_oxide::deflate::core::{CompressorOxide, create_comp_flags_from_zip_params};
use miniz_oxide::deflate::stream::deflate;
use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags::*};
use miniz_oxide::{MZFlush, MZStatus};
use serde::{Deserialize, Serialize};

const WINDOW: usize = 32 * 1024;
const BLOCK: usize = 64 * 1024;
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
const ZLIB_HEADER: [u8; 2] = [0x78, 0x9c];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Wrapper {
    Gzip,
    Zlib,
    Raw,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Codec {
    wrapper: Wrapper,
    state: State,
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeflateCodec")
            .field("wrapper", &self.wrapper)
            .field("decoding", &self.decoding())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum State {
    Encode(Encoder),
    Decode(Box<Decoder>),
}

impl Codec {
    pub(crate) fn new(format: Format, decoding: bool) -> Self {
        let wrapper = match format {
            Format::Gzip => Wrapper::Gzip,
            Format::Deflate => Wrapper::Zlib,
            Format::DeflateRaw => Wrapper::Raw,
            Format::Brotli => unreachable!("not a DEFLATE format"),
        };
        Self {
            wrapper,
            state: if decoding {
                State::Decode(Box::new(Decoder::new(wrapper)))
            } else {
                State::Encode(Encoder::default())
            },
        }
    }

    pub(crate) fn decoding(&self) -> bool {
        matches!(self.state, State::Decode(_))
    }

    pub(crate) fn process(&mut self, input: &[u8], finish: bool) -> Result<Vec<Vec<u8>>, ()> {
        match &mut self.state {
            State::Encode(encoder) => encoder.process(self.wrapper, input, finish),
            State::Decode(decoder) => decoder.process(self.wrapper, input, finish),
        }
    }

    pub(crate) fn valid_checkpoint(&self) -> bool {
        match &self.state {
            State::Encode(encoder) => {
                self.wrapper.valid_checksum(&encoder.sum)
                    && encoder.pending.len() < BLOCK
                    && (!encoder.finished || encoder.pending.is_empty())
                    && (encoder.started
                        || !encoder.finished
                            && encoder.pending.is_empty()
                            && encoder.sum == Checksum::default())
            }
            State::Decode(decoder) => decoder.valid_checkpoint(self.wrapper),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Checksum {
    crc: u32,
    adler: u32,
    size: u32,
}

impl Default for Checksum {
    fn default() -> Self {
        Self {
            crc: !0,
            adler: 1,
            size: 0,
        }
    }
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < table.len() {
        let mut v = i as u32;
        let mut bit = 0;
        while bit < 8 {
            v = (v >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(v & 1));
            bit += 1;
        }
        table[i] = v;
        i += 1;
    }
    table
};

impl Wrapper {
    fn valid_checksum(self, sum: &Checksum) -> bool {
        match self {
            Self::Gzip => sum.adler == 1,
            Self::Zlib => {
                sum.crc == !0
                    && sum.size == 0
                    && (sum.adler & 0xffff) < 65521
                    && (sum.adler >> 16) < 65521
            }
            Self::Raw => *sum == Checksum::default(),
        }
    }
}

fn crc_update(mut crc: u32, input: &[u8]) -> u32 {
    for &byte in input {
        crc = (crc >> 8) ^ CRC_TABLE[((crc as u8) ^ byte) as usize];
    }
    crc
}

impl Checksum {
    fn update(&mut self, wrapper: Wrapper, input: &[u8]) {
        match wrapper {
            Wrapper::Gzip => {
                self.crc = crc_update(self.crc, input);
                self.size = self.size.wrapping_add(input.len() as u32);
            }
            Wrapper::Zlib => {
                let (mut a, mut b) = (self.adler & 0xffff, self.adler >> 16);
                for chunk in input.chunks(5552) {
                    for &byte in chunk {
                        a += u32::from(byte);
                        b += a;
                    }
                    a %= 65521;
                    b %= 65521;
                }
                self.adler = (b << 16) | a;
            }
            Wrapper::Raw => {}
        }
    }

    fn trailer(&self, wrapper: Wrapper) -> ([u8; 8], usize) {
        let mut bytes = [0; 8];
        let len = match wrapper {
            Wrapper::Gzip => {
                bytes[..4].copy_from_slice(&(!self.crc).to_le_bytes());
                bytes[4..].copy_from_slice(&self.size.to_le_bytes());
                8
            }
            Wrapper::Zlib => {
                bytes[..4].copy_from_slice(&self.adler.to_be_bytes());
                4
            }
            Wrapper::Raw => 0,
        };
        (bytes, len)
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Encoder {
    sum: Checksum,
    started: bool,
    finished: bool,
    pending: Vec<u8>,
}

impl Encoder {
    fn process(
        &mut self,
        wrapper: Wrapper,
        mut input: &[u8],
        finish: bool,
    ) -> Result<Vec<Vec<u8>>, ()> {
        if self.finished {
            return if input.is_empty() {
                Ok(Vec::new())
            } else {
                Err(())
            };
        }
        let mut output = Vec::new();
        if !self.started {
            self.started = true;
            match wrapper {
                Wrapper::Gzip => output.push(GZIP_HEADER.to_vec()),
                Wrapper::Zlib => output.push(ZLIB_HEADER.to_vec()),
                Wrapper::Raw => {}
            }
        }
        self.sum.update(wrapper, input);

        // Each nonfinal segment ends in a byte-aligned Sync flush. Starting the
        // next segment with an empty match dictionary requires no checkpoint of
        // miniz's encoder internals and still produces one continuous member.
        let mut compressor = None;
        if !self.pending.is_empty() {
            let count = (BLOCK - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..count]);
            input = &input[count..];
            if self.pending.len() == BLOCK {
                encode_segment(&mut compressor, &self.pending, false, &mut output)?;
                self.pending.clear();
            }
        }
        while input.len() >= BLOCK {
            let (segment, rest) = input.split_at(BLOCK);
            encode_segment(&mut compressor, segment, false, &mut output)?;
            input = rest;
        }
        if finish {
            let tail = if self.pending.is_empty() {
                input
            } else {
                &self.pending
            };
            encode_segment(&mut compressor, tail, true, &mut output)?;
            self.pending.clear();
            self.finish(wrapper, &mut output);
        } else {
            self.pending.extend_from_slice(input);
        }
        Ok(output)
    }

    fn finish(&mut self, wrapper: Wrapper, output: &mut Vec<Vec<u8>>) {
        self.finished = true;
        let (trailer, len) = self.sum.trailer(wrapper);
        if len != 0 {
            output.push(trailer[..len].to_vec());
        }
    }
}

fn encode_segment(
    compressor: &mut Option<CompressorOxide>,
    input: &[u8],
    last: bool,
    output: &mut Vec<Vec<u8>>,
) -> Result<(), ()> {
    let encoder = if let Some(encoder) = compressor {
        encoder.reset();
        encoder
    } else {
        compressor.insert(CompressorOxide::new(create_comp_flags_from_zip_params(
            6, -15, 0,
        )))
    };
    // Space for the data plus stored-block overhead, with enough slack to
    // ensure a completed Sync flush never needs to be requested twice.
    let mut bytes = vec![0; input.len() + 1024];
    let result = deflate(
        encoder,
        input,
        &mut bytes,
        if last { MZFlush::Finish } else { MZFlush::Sync },
    );
    if result.bytes_consumed != input.len()
        || result.bytes_written == bytes.len()
        || result.status
            != Ok(if last {
                MZStatus::StreamEnd
            } else {
                MZStatus::Ok
            })
    {
        return Err(());
    }
    bytes.truncate(result.bytes_written);
    output.push(bytes);
    Ok(())
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum Phase {
    Header { bytes: [u8; 10], used: u8 },
    GzipExtraLength { bytes: [u8; 2], used: u8 },
    GzipExtra(u16),
    GzipName,
    GzipComment,
    GzipHeaderCrc { bytes: [u8; 2], used: u8 },
    Body,
    Trailer { bytes: [u8; 8], used: u8 },
    Done,
    Failed,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Decoder {
    phase: Phase,
    core: Box<DecompressorOxide>,
    history: Vec<u8>,
    pos: usize,
    wrapped: bool,
    sum: Checksum,
    gzip_flags: u8,
    header_crc: u32,
}

impl Decoder {
    fn new(wrapper: Wrapper) -> Self {
        Self {
            phase: if wrapper == Wrapper::Raw {
                Phase::Body
            } else {
                Phase::Header {
                    bytes: [0; 10],
                    used: 0,
                }
            },
            core: Box::default(),
            history: if wrapper == Wrapper::Zlib {
                Vec::new()
            } else {
                vec![0; WINDOW]
            },
            pos: 0,
            wrapped: false,
            sum: Checksum::default(),
            gzip_flags: 0,
            header_crc: !0,
        }
    }

    fn valid_checkpoint(&self, wrapper: Wrapper) -> bool {
        if matches!(self.phase, Phase::Failed) {
            return self.history.is_empty()
                && self.pos == 0
                && !self.wrapped
                && self.gzip_flags == 0
                && self.header_crc == !0
                && self.sum == Checksum::default()
                && self.core.valid_raw_checkpoint(true, false);
        }
        let header = matches!(
            self.phase,
            Phase::Header { .. }
                | Phase::GzipExtraLength { .. }
                | Phase::GzipExtra(_)
                | Phase::GzipName
                | Phase::GzipComment
                | Phase::GzipHeaderCrc { .. }
        );
        let initial = self.core.raw_checkpoint_is_initial();
        let complete = matches!(self.phase, Phase::Trailer { .. } | Phase::Done);
        let history_valid = match wrapper {
            Wrapper::Zlib if header => self.history.is_empty(),
            Wrapper::Zlib => {
                (256..=WINDOW).contains(&self.history.len()) && self.history.len().is_power_of_two()
            }
            _ => self.history.len() == WINDOW,
        };
        if !history_valid
            || self.pos >= self.history.len().max(1)
            || (!self.wrapped && self.history[self.pos..].iter().any(|&byte| byte != 0))
            || (header && !initial)
            || (initial && (self.pos != 0 || self.wrapped || self.sum != Checksum::default()))
            || !wrapper.valid_checksum(&self.sum)
            || (wrapper == Wrapper::Gzip && self.sum.size as usize % WINDOW != self.pos)
            || (wrapper == Wrapper::Gzip && !self.wrapped && self.sum.size as usize != self.pos)
            || (wrapper != Wrapper::Gzip && (self.gzip_flags != 0 || self.header_crc != !0))
            || !self.core.valid_raw_checkpoint(initial, complete)
        {
            return false;
        }
        let valid_field = |allowed: u8| wrapper == Wrapper::Gzip && self.gzip_flags & !allowed == 0;
        match self.phase {
            Phase::Header { bytes, used } => {
                let n = usize::from(used);
                match wrapper {
                    Wrapper::Gzip => {
                        valid_field(0)
                            && n < 10
                            && bytes[n..].iter().all(|&byte| byte == 0)
                            && bytes[..n.min(3)] == GZIP_HEADER[..n.min(3)]
                            && (n < 4 || bytes[3] & 0xe0 == 0)
                            && self.header_crc == crc_update(!0, &bytes[..n])
                    }
                    Wrapper::Zlib => {
                        n < 2
                            && bytes[n..].iter().all(|&byte| byte == 0)
                            && (n == 0 || (bytes[0] & 15 == 8 && bytes[0] >> 4 <= 7))
                    }
                    Wrapper::Raw => false,
                }
            }
            Phase::GzipExtraLength { bytes, used } => {
                valid_field(0x1a)
                    && used < 2
                    && bytes[usize::from(used)..].iter().all(|&byte| byte == 0)
            }
            Phase::GzipExtra(left) => valid_field(0x1a) && left != 0,
            Phase::GzipName => valid_field(0x12),
            Phase::GzipComment => valid_field(0x02),
            Phase::GzipHeaderCrc { bytes, used } => {
                valid_field(0)
                    && used < 2
                    && bytes[usize::from(used)..].iter().all(|&byte| byte == 0)
                    && bytes[..usize::from(used)]
                        == (!self.header_crc as u16).to_le_bytes()[..usize::from(used)]
            }
            Phase::Trailer { bytes, used } => {
                let (expected, len) = self.sum.trailer(wrapper);
                let n = usize::from(used);
                self.gzip_flags == 0
                    && n < len
                    && bytes[..n] == expected[..n]
                    && bytes[n..].iter().all(|&byte| byte == 0)
            }
            Phase::Body | Phase::Done => self.gzip_flags == 0,
            Phase::Failed => unreachable!(),
        }
    }

    fn next_gzip_field(&mut self) -> Phase {
        if self.gzip_flags & 4 != 0 {
            self.gzip_flags &= !4;
            Phase::GzipExtraLength {
                bytes: [0; 2],
                used: 0,
            }
        } else if self.gzip_flags & 8 != 0 {
            self.gzip_flags &= !8;
            Phase::GzipName
        } else if self.gzip_flags & 16 != 0 {
            self.gzip_flags &= !16;
            Phase::GzipComment
        } else if self.gzip_flags & 2 != 0 {
            self.gzip_flags &= !2;
            Phase::GzipHeaderCrc {
                bytes: [0; 2],
                used: 0,
            }
        } else {
            Phase::Body
        }
    }

    fn process(
        &mut self,
        wrapper: Wrapper,
        input: &[u8],
        finish: bool,
    ) -> Result<Vec<Vec<u8>>, ()> {
        let result = self.run(wrapper, input, finish);
        if result.is_err() {
            self.history.clear();
            *self.core = DecompressorOxide::default();
            self.pos = 0;
            self.wrapped = false;
            self.sum = Checksum::default();
            self.gzip_flags = 0;
            self.header_crc = !0;
            self.phase = Phase::Failed;
        }
        result
    }

    fn run(
        &mut self,
        wrapper: Wrapper,
        mut input: &[u8],
        finish: bool,
    ) -> Result<Vec<Vec<u8>>, ()> {
        let mut output = Vec::new();
        loop {
            match &mut self.phase {
                Phase::Header { bytes, used } => {
                    let len = if wrapper == Wrapper::Gzip { 10 } else { 2 };
                    let read = fill(&mut bytes[..len], used, &mut input);
                    if wrapper == Wrapper::Gzip {
                        self.header_crc = crc_update(self.header_crc, read);
                        let have = usize::from(*used);
                        if bytes[..have.min(3)] != GZIP_HEADER[..have.min(3)]
                            || (have >= 4 && bytes[3] & 0xe0 != 0)
                        {
                            return Err(());
                        }
                        if have < len {
                            break;
                        }
                        self.gzip_flags = bytes[3] & !1;
                        self.phase = self.next_gzip_field();
                    } else {
                        let cmf = bytes[0];
                        if *used >= 1 && (cmf & 15 != 8 || cmf >> 4 > 7) {
                            return Err(());
                        }
                        if usize::from(*used) < len {
                            break;
                        }
                        if bytes[1] & 0x20 != 0
                            || !u16::from_be_bytes([cmf, bytes[1]]).is_multiple_of(31)
                        {
                            return Err(());
                        }
                        self.history = vec![0; 1 << (8 + (cmf >> 4))];
                        self.phase = Phase::Body;
                    }
                }
                Phase::GzipExtraLength { bytes, used } => {
                    self.header_crc = crc_update(self.header_crc, fill(bytes, used, &mut input));
                    if *used < 2 {
                        break;
                    }
                    let len = u16::from_le_bytes(*bytes);
                    self.phase = if len == 0 {
                        self.next_gzip_field()
                    } else {
                        Phase::GzipExtra(len)
                    };
                }
                Phase::GzipExtra(left) => {
                    let len = usize::from(*left).min(input.len());
                    let (read, rest) = input.split_at(len);
                    self.header_crc = crc_update(self.header_crc, read);
                    input = rest;
                    *left -= len as u16;
                    if *left != 0 {
                        break;
                    }
                    self.phase = self.next_gzip_field();
                }
                Phase::GzipName | Phase::GzipComment => {
                    let end = input.iter().position(|&byte| byte == 0);
                    let (read, rest) = input.split_at(end.map_or(input.len(), |pos| pos + 1));
                    self.header_crc = crc_update(self.header_crc, read);
                    input = rest;
                    if end.is_none() {
                        break;
                    }
                    self.phase = self.next_gzip_field();
                }
                Phase::GzipHeaderCrc { bytes, used } => {
                    fill(bytes, used, &mut input);
                    let expected = (!self.header_crc as u16).to_le_bytes();
                    if bytes[..usize::from(*used)] != expected[..usize::from(*used)] {
                        return Err(());
                    }
                    if *used < 2 {
                        break;
                    }
                    self.phase = Phase::Body;
                }
                Phase::Body => {
                    let mut flags = TINFL_FLAG_IGNORE_ADLER32;
                    if !finish {
                        flags |= TINFL_FLAG_HAS_MORE_INPUT;
                    }
                    // Before the first wrap, references before the beginning of
                    // the output are invalid, even though the ring is allocated.
                    if !self.wrapped {
                        flags |= TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
                    }
                    let (status, consumed, written) =
                        decompress(&mut self.core, input, &mut self.history, self.pos, flags);
                    input = &input[consumed..];
                    if written != 0 {
                        let end = self.pos + written;
                        let decoded = &self.history[self.pos..end];
                        self.sum.update(wrapper, decoded);
                        output.push(decoded.to_vec());
                        if end == self.history.len() {
                            self.wrapped = true;
                            self.pos = 0;
                        } else {
                            self.pos = end;
                        }
                    }
                    match status {
                        TINFLStatus::Done => {
                            self.phase = if wrapper == Wrapper::Raw {
                                Phase::Done
                            } else {
                                Phase::Trailer {
                                    bytes: [0; 8],
                                    used: 0,
                                }
                            };
                        }
                        TINFLStatus::NeedsMoreInput => break,
                        TINFLStatus::HasMoreOutput if written != 0 => continue,
                        _ => return Err(()),
                    }
                }
                Phase::Trailer { bytes, used } => {
                    let (expected, len) = self.sum.trailer(wrapper);
                    fill(&mut bytes[..len], used, &mut input);
                    let have = usize::from(*used);
                    if bytes[..have] != expected[..have] {
                        return Err(());
                    }
                    if have < len {
                        break;
                    }
                    self.phase = Phase::Done;
                }
                Phase::Done => {
                    return if input.is_empty() {
                        Ok(output)
                    } else {
                        Err(())
                    };
                }
                Phase::Failed => return Err(()),
            }
        }
        if finish { Err(()) } else { Ok(output) }
    }
}

fn fill<'a>(out: &mut [u8], used: &mut u8, input: &mut &'a [u8]) -> &'a [u8] {
    let start = usize::from(*used);
    let len = (out.len() - start).min(input.len());
    let (read, rest) = input.split_at(len);
    out[start..start + len].copy_from_slice(read);
    *used += len as u8;
    *input = rest;
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"wrapper semantics\n";
    // Fixtures produced with zlib. The gzip member has all four optional fields.
    const RAW: &[u8] =
        b"\x2b\x2f\x4a\x2c\x28\x48\x2d\x52\x28\x4e\xcd\x4d\xcc\x2b\xc9\x4c\x2e\xe6\x02\x00";
    const ZLIB: &[u8] = b"\x78\x9c\x2b\x2f\x4a\x2c\x28\x48\x2d\x52\x28\x4e\xcd\x4d\xcc\x2b\xc9\x4c\x2e\xe6\x02\x00\x45\x4a\x06\xf3";
    const GZIP: &[u8] = b"\x1f\x8b\x08\x1e\x00\x00\x00\x00\x00\xff\x03\x00abcname\0comment\0\x78\x18\x2b\x2f\x4a\x2c\x28\x48\x2d\x52\x28\x4e\xcd\x4d\xcc\x2b\xc9\x4c\x2e\xe6\x02\x00\x6e\xad\x8f\x62\x12\x00\x00\x00";

    #[test]
    fn validates_exactly_one_complete_member() {
        for (format, fixture) in [
            (Format::Gzip, GZIP),
            (Format::Deflate, ZLIB),
            (Format::DeflateRaw, RAW),
        ] {
            let mut decoder = Codec::new(format, true);
            let mut decoded = Vec::new();
            for byte in fixture {
                decoded.extend(
                    decoder
                        .process(&[*byte], false)
                        .unwrap()
                        .into_iter()
                        .flatten(),
                );
                assert!(decoder.valid_checkpoint());
            }
            assert!(decoder.process(&[], true).unwrap().is_empty());
            assert_eq!(decoded, TEXT);
            assert!(decoder.process(b"x", false).is_err());

            for end in 0..fixture.len() {
                let mut decoder = Codec::new(format, true);
                decoder.process(&fixture[..end], false).unwrap();
                assert!(decoder.process(&[], true).is_err());
            }
            let mut decoder = Codec::new(format, true);
            assert!(
                decoder
                    .process(&[fixture, fixture].concat(), false)
                    .is_err()
            );
        }
    }

    #[test]
    fn finishes_an_empty_member() {
        for format in [Format::Gzip, Format::Deflate, Format::DeflateRaw] {
            let bytes: Vec<_> = Codec::new(format, false)
                .process(&[], true)
                .unwrap()
                .into_iter()
                .flatten()
                .collect();
            assert!(!bytes.is_empty());
            assert!(
                Codec::new(format, true)
                    .process(&bytes, true)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn rejects_invalid_wrappers_and_backreferences() {
        for (format, fixture, offsets) in [
            (
                Format::Gzip,
                GZIP,
                &[0, 2, 3, 27, GZIP.len() - 8, GZIP.len() - 4][..],
            ),
            (Format::Deflate, ZLIB, &[0, 1, ZLIB.len() - 1][..]),
        ] {
            for &offset in offsets {
                let mut bytes = fixture.to_vec();
                bytes[offset] ^= 0x80;
                assert!(Codec::new(format, true).process(&bytes, true).is_err());
            }
        }
        // Valid zlib FCHECK, but FDICT is set (preset dictionaries are unsupported).
        assert!(
            Codec::new(Format::Deflate, true)
                .process(&[0x78, 0x20], false)
                .is_err()
        );
        // Fixed Huffman length-3/distance-1 before any literal output.
        assert!(
            Codec::new(Format::DeflateRaw, true)
                .process(&[0x03, 0x02, 0x00], true)
                .is_err()
        );
    }

    #[test]
    fn resumes_both_directions_and_wraps_history() {
        let text: Vec<_> = (0..2 * BLOCK + 19).map(|i| (i % 251) as u8).collect();
        for format in [Format::Gzip, Format::Deflate, Format::DeflateRaw] {
            let mut encoder = Codec::new(format, false);
            let first = encoder.process(&text[..BLOCK + 17], false).unwrap();
            assert!(!first.is_empty());
            encoder = round_trip(&encoder);
            let last = encoder.process(&text[BLOCK + 17..], true).unwrap();
            let encoded: Vec<_> = first.into_iter().chain(last).flatten().collect();
            let mut contiguous = Codec::new(format, false);
            assert_eq!(
                encoded,
                contiguous
                    .process(&text, true)
                    .unwrap()
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            );
            assert!(encoded.len() < text.len() / 4);
            let mut decoder = Codec::new(format, true);
            let mid = encoded.len() / 2;
            let first = decoder.process(&encoded[..mid], false).unwrap();
            assert!(!first.is_empty());
            decoder = round_trip(&decoder);
            let last = decoder.process(&encoded[mid..], true).unwrap();
            assert_eq!(
                first.into_iter().chain(last).flatten().collect::<Vec<_>>(),
                text
            );
        }
    }

    #[test]
    fn rejects_forged_core_and_wrapper_cursors() {
        let mut decoder = Codec::new(Format::DeflateRaw, true);
        decoder.process(&RAW[..6], false).unwrap();
        assert!(decoder.valid_checkpoint());
        let snapshot = serde_json::to_value(&decoder).unwrap();
        let mut bad_bits = snapshot.clone();
        bad_bits["state"]["Decode"]["core"]["num_bits"] = 64.into();
        let mut bad_tree = snapshot.clone();
        bad_tree["state"]["Decode"]["core"]["tables"][0]["look_up"][0] = (-32768).into();
        let mut bad_cursor = snapshot;
        bad_cursor["state"]["Decode"]["pos"] = WINDOW.into();
        for value in [bad_bits, bad_tree, bad_cursor] {
            let decoder: Codec = serde_json::from_value(value).unwrap();
            assert!(!decoder.valid_checkpoint());
        }
    }

    fn round_trip(codec: &Codec) -> Codec {
        let decoded = serde_json::from_slice::<Codec>(&serde_json::to_vec(codec).unwrap()).unwrap();
        assert!(decoded.valid_checkpoint());
        assert_eq!(&decoded, codec);
        decoded
    }
}
