//! Single-stream Brotli encoding and decoding. Checkpoints contain the live bit
//! cursor, history window and parser state, never a replay log of source bytes.

#[path = "brotli/decoder.rs"]
mod decoder;

use super::Format;
use brotli::enc::StandardAlloc;
use brotli::enc::encode::{BrotliEncoderOperation, BrotliEncoderStateStruct};
use decoder::Decoder;
use serde::{Deserialize, Serialize};

const BLOCK: usize = 16 * 1024;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Codec(State);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum State {
    Encode(Encoder),
    Decode(Decoder),
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrotliCodec")
            .field("decoding", &self.decoding())
            .finish_non_exhaustive()
    }
}

impl Codec {
    pub(crate) fn new(_format: Format, decoding: bool) -> Self {
        Self(if decoding {
            State::Decode(Decoder::new())
        } else {
            State::Encode(Encoder::Header)
        })
    }

    pub(crate) fn decoding(&self) -> bool {
        matches!(self.0, State::Decode(_))
    }

    pub(crate) fn process(&mut self, input: &[u8], finish: bool) -> Result<Vec<Vec<u8>>, ()> {
        match &mut self.0 {
            State::Encode(encoder) => encoder.process(input, finish),
            State::Decode(decoder) => decoder.process(input, finish),
        }
    }

    pub(crate) fn valid_checkpoint(&self) -> bool {
        match &self.0 {
            State::Encode(encoder) => encoder.valid_checkpoint(),
            State::Decode(decoder) => decoder.valid_checkpoint(),
        }
    }
}

// Quality 1 works in independent *metablocks*, not independent Brotli streams.
// Its two-pass stream path consumes each supplied fragment immediately. Hash
// tables are cleared before each fragment, no previous window is read, and we
// always supply enough output space to avoid pending storage. The sole stream
// continuation between fragments is the partial output byte. The fixed upstream
// version and parameters are part of this checkpoint's meaning.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Encoder {
    Header,
    Pending { bits: u8, byte: u8 },
    Done,
}

impl Encoder {
    fn valid_checkpoint(self) -> bool {
        match self {
            Self::Pending { bits, byte } => bits < 8 && u16::from(byte) < (1 << bits),
            _ => true,
        }
    }

    fn process(&mut self, input: &[u8], finish: bool) -> Result<Vec<Vec<u8>>, ()> {
        if matches!(self, Self::Done) || !self.valid_checkpoint() {
            return Err(());
        }
        if input.is_empty() && !finish {
            return Ok(Vec::new());
        }
        use BrotliEncoderOperation::{BROTLI_OPERATION_FINISH, BROTLI_OPERATION_PROCESS};

        let mut encoder = BrotliEncoderStateStruct::new(StandardAlloc::default());
        encoder.params.quality = 1;
        encoder.params.lgwin = 18;
        let run = |encoder: &mut BrotliEncoderStateStruct<StandardAlloc>,
                   input: &[u8],
                   finish: bool|
         -> Result<Vec<u8>, ()> {
            let mut available_in = input.len();
            let mut input_offset = 0;
            // The upstream fast stream path requires this much for direct output.
            let mut output = vec![0; 2 * input.len() + 503];
            let mut available_out = output.len();
            let mut output_offset = 0;
            if !encoder.compress_stream(
                if finish {
                    BROTLI_OPERATION_FINISH
                } else {
                    BROTLI_OPERATION_PROCESS
                },
                &mut available_in,
                input,
                &mut input_offset,
                &mut available_out,
                &mut output,
                &mut output_offset,
                &mut None,
                &mut |_, _, _, _| {},
            ) || available_in != 0
                || encoder.has_more_output()
                || (finish && !encoder.is_finished())
            {
                return Err(());
            }
            output.truncate(output_offset);
            Ok(output)
        };
        if let Self::Pending { bits, byte } = *self {
            // Initialize the fixed parameters without emitting a new header.
            run(&mut encoder, &[], false)?;
            encoder.last_bytes_bits_ = bits;
            encoder.last_bytes_ = u16::from(byte);
        }

        let mut chunks = Vec::new();
        let mut parts = input.chunks(BLOCK).peekable();
        while let Some(part) = parts.next() {
            let output = run(&mut encoder, part, finish && parts.peek().is_none())?;
            if !output.is_empty() {
                chunks.push(output);
            }
        }
        if input.is_empty() {
            let output = run(&mut encoder, &[], true)?;
            if !output.is_empty() {
                chunks.push(output);
            }
        }
        *self = if finish {
            Self::Done
        } else {
            let bits = encoder.last_bytes_bits_;
            Self::Pending {
                bits,
                byte: (encoder.last_bytes_ & ((1 << bits) - 1)) as u8,
            }
        };
        Ok(chunks)
    }
}
