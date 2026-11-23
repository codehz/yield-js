use brotli_decompressor::context::kContextLookup;
use brotli_decompressor::state::*;
use brotli_decompressor::{
    Allocator, BrotliDecompressStream, BrotliResult, BrotliState, HuffmanCode, HuffmanTreeGroup,
    SliceWrapper, SliceWrapperMut,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::borrow::Cow;

const OUTPUT_BLOCK: usize = 16 * 1024;
const INPUT_BLOCK: usize = 1024 * 1024;
const HUFFMAN_TABLE: usize = 1080;
const RING_SLACK: usize = 542 + 24;

#[derive(Default)]
struct Heap;

struct Memory<T>(Vec<T>);

impl<T> Default for Memory<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> SliceWrapper<T> for Memory<T> {
    fn slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> SliceWrapperMut<T> for Memory<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl<T: Copy + Default> Allocator<T> for Heap {
    type AllocatedMemory = Memory<T>;

    fn alloc_cell(&mut self, len: usize) -> Memory<T> {
        Memory(vec![T::default(); len])
    }

    fn free_cell(&mut self, _: Memory<T>) {}
}

type Live = BrotliState<Heap, Heap, Heap>;
type TreeGroup = HuffmanTreeGroup<Heap, Heap>;

pub(super) struct Decoder(Option<Box<Live>>);

impl Decoder {
    pub(super) fn new() -> Self {
        Self(Some(Box::new(Live::new_strict(Heap, Heap, Heap))))
    }

    pub(super) fn process(&mut self, input: &[u8], finish: bool) -> Result<Vec<Vec<u8>>, ()> {
        let Some(state) = self.0.as_deref_mut() else {
            return if input.is_empty() {
                Ok(Vec::new())
            } else {
                Err(())
            };
        };
        if input.is_empty() {
            return if finish { Err(()) } else { Ok(Vec::new()) };
        }
        let mut chunks = Vec::new();
        let mut pieces = input.chunks(INPUT_BLOCK).peekable();
        let mut output = [0; OUTPUT_BLOCK];
        while let Some(piece) = pieces.next() {
            let mut available_in = piece.len();
            let mut input_offset = 0;
            loop {
                let mut available_out = output.len();
                let mut output_offset = 0;
                let result = BrotliDecompressStream(
                    &mut available_in,
                    &mut input_offset,
                    piece,
                    &mut available_out,
                    &mut output_offset,
                    &mut output,
                    &mut 0,
                    state,
                );
                if output_offset != 0 {
                    chunks.push(output[..output_offset].to_vec());
                }
                match result {
                    BrotliResult::ResultFailure => return Err(()),
                    BrotliResult::ResultSuccess => {
                        if available_in != 0 || pieces.peek().is_some() {
                            return Err(());
                        }
                        self.0 = None;
                        return Ok(chunks);
                    }
                    BrotliResult::NeedsMoreInput => {
                        if available_in != 0 {
                            return Err(());
                        }
                        // Even on NeedsMoreInput, a full output buffer can leave
                        // additional decoded output waiting in the ring.
                        if available_out != 0 {
                            break;
                        }
                    }
                    BrotliResult::NeedsMoreOutput => {
                        if output_offset == 0 {
                            return Err(());
                        }
                    }
                }
            }
        }
        if finish { Err(()) } else { Ok(chunks) }
    }

    pub(super) fn valid_checkpoint(&self) -> bool {
        self.0
            .as_deref()
            .is_none_or(|state| Record::from(state).valid())
    }
}

impl Clone for Decoder {
    fn clone(&self) -> Self {
        Self(
            self.0
                .as_deref()
                .map(|live| Box::new(Record::from(live).into_live())),
        )
    }
}

impl PartialEq for Decoder {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_deref().map(Record::from) == other.0.as_deref().map(Record::from)
    }
}

impl Serialize for Decoder {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_deref().map(Record::from).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Decoder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = Option::<Record<'_>>::deserialize(deserializer)?;
        if record.as_ref().is_some_and(|record| !record.valid()) {
            return Err(D::Error::custom("invalid Brotli decoder checkpoint"));
        }
        Ok(Self(record.map(|record| Box::new(record.into_live()))))
    }
}

#[derive(PartialEq, Serialize, Deserialize)]
struct Group<'a> {
    htrees: Cow<'a, [u32]>,
    codes: Cow<'a, [HuffmanCode]>,
    alphabet_size: u16,
    max_symbol: u16,
    num_htrees: u16,
}

impl<'a> From<&'a TreeGroup> for Group<'a> {
    fn from(group: &'a TreeGroup) -> Self {
        Self {
            htrees: Cow::Borrowed(group.htrees.slice()),
            codes: Cow::Borrowed(group.codes.slice()),
            alphabet_size: group.alphabet_size,
            max_symbol: group.max_symbol,
            num_htrees: group.num_htrees,
        }
    }
}

impl Group<'_> {
    fn into_live(self) -> TreeGroup {
        TreeGroup {
            htrees: Memory(self.htrees.into_owned()),
            codes: Memory(self.codes.into_owned()),
            alphabet_size: self.alphabet_size,
            max_symbol: self.max_symbol,
            num_htrees: self.num_htrees,
        }
    }

    fn valid(&self) -> bool {
        let trees = usize::from(self.num_htrees);
        if self.htrees.is_empty() && self.codes.is_empty() {
            // Upstream frees buffers at a metablock boundary but leaves
            // the previous alphabet and tree count in their scalar fields.
            return trees <= 256
                && self.max_symbol <= self.alphabet_size
                && self.alphabet_size <= 704;
        }
        trees <= 256
            && self.max_symbol <= self.alphabet_size
            && self.alphabet_size <= 704
            && self.htrees.len() == trees
            && self.codes.len() == trees * HUFFMAN_TABLE
            && self.htrees[0] == 0
            // While building a group, unfilled offsets remain zero.
            && self.htrees[1..].windows(2).all(|pair| pair[1] == 0 || pair[0] < pair[1])
            && self.htrees.iter().all(|&offset| (offset as usize) <= self.codes.len() - 256)
            && valid_codes(&self.codes)
    }
}

fn valid_codes(codes: &[HuffmanCode]) -> bool {
    codes
        .iter()
        .all(|code| code.bits <= 15 && usize::from(code.value) < HUFFMAN_TABLE)
}

// Explicit, versioned-through-the-VM-schema projection of the pinned upstream
// state. Borrow the backing stores during serialization and equality; the
// deserialized Vecs transfer ownership straight to the decoder allocator.
// Allocation policy and the built-in RFC dictionary are reconstructed, not
// serialized. External and large-window dictionaries are not part of RFC 7932.
macro_rules! checkpoint {
    (
        scalars { $($scalar:ident : $ty:ty => $($scalar_path:ident).+),+ $(,)? }
        buffers { $($buffer:ident : $element:ty => $($buffer_path:ident).+),+ $(,)? }
    ) => {
        #[derive(PartialEq, Serialize, Deserialize)]
        struct Record<'a> {
            $($scalar: $ty,)+
            $($buffer: Cow<'a, [$element]>,)+
            literal_hgroup: Group<'a>,
            insert_copy_hgroup: Group<'a>,
            distance_hgroup: Group<'a>,
            symbols_lists_array: Cow<'a, [u16]>,
            mtf: Cow<'a, [u8]>,
            context_mode: u8,
        }

        impl<'a> From<&'a Live> for Record<'a> {
            fn from(live: &'a Live) -> Self {
                Self {
                    $($scalar: live.$($scalar_path).+,)+
                    $($buffer: Cow::Borrowed(live.$($buffer_path).+.slice()),)+
                    literal_hgroup: (&live.literal_hgroup).into(),
                    insert_copy_hgroup: (&live.insert_copy_hgroup).into(),
                    distance_hgroup: (&live.distance_hgroup).into(),
                    symbols_lists_array: Cow::Borrowed(&live.symbols_lists_array),
                    mtf: Cow::Borrowed(live.mtf_or_error_string.as_ref().unwrap_or_else(|v| v)),
                    context_mode: kContextLookup.iter()
                        .position(|table| std::ptr::eq(table, live.context_lookup))
                        .expect("Brotli uses a built-in context table") as u8,
                }
            }
        }

        impl Record<'_> {
            fn into_live(self) -> Live {
                let mut live = Live::new_strict(Heap, Heap, Heap);
                $(live.$($scalar_path).+ = self.$scalar;)+
                $(live.$($buffer_path).+ = Memory(self.$buffer.into_owned());)+
                live.literal_hgroup = self.literal_hgroup.into_live();
                live.insert_copy_hgroup = self.insert_copy_hgroup.into_live();
                live.distance_hgroup = self.distance_hgroup.into_live();
                live.symbols_lists_array.copy_from_slice(&self.symbols_lists_array);
                live.mtf_or_error_string = Ok(self.mtf.as_ref().try_into().expect("validated MTF size"));
                live.context_lookup = &kContextLookup[usize::from(self.context_mode)];
                live
            }
        }
    };
}

checkpoint! {
    scalars {
        state: BrotliRunningState => state,
        loop_counter: i32 => loop_counter,
        bit_word: u64 => br.val_,
        bit_pos: u32 => br.bit_pos_,
        buffered_avail: u32 => br.avail_in,
        buffer: [u8; 8] => buffer,
        buffer_length: u32 => buffer_length,
        pos: i32 => pos,
        max_backward_distance: i32 => max_backward_distance,
        max_distance: i32 => max_distance,
        ringbuffer_size: i32 => ringbuffer_size,
        ringbuffer_mask: i32 => ringbuffer_mask,
        dist_rb_idx: i32 => dist_rb_idx,
        dist_rb: [i32; 4] => dist_rb,
        htree_command_index: u16 => htree_command_index,
        context_map_slice_index: usize => context_map_slice_index,
        dist_context_map_slice_index: usize => dist_context_map_slice_index,
        sub_loop_counter: u32 => sub_loop_counter,
        trivial_literal_context: i32 => trivial_literal_context,
        distance_context: i32 => distance_context,
        meta_block_remaining_len: i32 => meta_block_remaining_len,
        substate_read_block_length: BrotliRunningReadBlockLengthState => block_type_length_state.substate_read_block_length,
        num_block_types: [u32; 3] => block_type_length_state.num_block_types,
        block_length_index: u32 => block_type_length_state.block_length_index,
        block_length: [u32; 3] => block_type_length_state.block_length,
        block_type_rb: [u32; 6] => block_type_length_state.block_type_rb,
        distance_postfix_bits: u32 => distance_postfix_bits,
        num_direct_distance_codes: u32 => num_direct_distance_codes,
        distance_postfix_mask: i32 => distance_postfix_mask,
        num_dist_htrees: u32 => num_dist_htrees,
        literal_htree_index: u8 => literal_htree_index,
        dist_htree_index: u8 => dist_htree_index,
        should_wrap_ringbuffer: bool => should_wrap_ringbuffer,
        error_code: BrotliDecoderErrorCode => error_code,
        repeat_code_len: u32 => repeat_code_len,
        prev_code_len: u32 => prev_code_len,
        copy_length: i32 => copy_length,
        distance_code: i32 => distance_code,
        rb_roundtrips: u64 => rb_roundtrips,
        partial_pos_out: u64 => partial_pos_out,
        symbol: u32 => symbol,
        repeat: u32 => repeat,
        space: u32 => space,
        table: [HuffmanCode; 32] => table,
        symbol_lists_index: usize => symbol_lists_index,
        next_symbol: [i32; 32] => next_symbol,
        code_length_code_lengths: [u8; 18] => code_length_code_lengths,
        code_length_histo: [u16; 16] => code_length_histo,
        htree_index: i32 => htree_index,
        htree_next_offset: u32 => htree_next_offset,
        context_index: u32 => context_index,
        max_run_length_prefix: u32 => max_run_length_prefix,
        code: u32 => code,
        mtf_upper_bound: u32 => mtf_upper_bound,
        substate_metablock_header: BrotliRunningMetablockHeaderState => substate_metablock_header,
        substate_tree_group: BrotliRunningTreeGroupState => substate_tree_group,
        substate_context_map: BrotliRunningContextMapState => substate_context_map,
        substate_uncompressed: BrotliRunningUncompressedState => substate_uncompressed,
        substate_huffman: BrotliRunningHuffmanState => substate_huffman,
        substate_decode_uint8: BrotliRunningDecodeUint8State => substate_decode_uint8,
        is_last_metablock: u8 => is_last_metablock,
        is_uncompressed: u8 => is_uncompressed,
        is_metadata: u8 => is_metadata,
        size_nibbles: u8 => size_nibbles,
        window_bits: u32 => window_bits,
        num_literal_htrees: u32 => num_literal_htrees,
        trivial_literal_contexts: [u32; 8] => trivial_literal_contexts,
    }
    buffers {
        ringbuffer: u8 => ringbuffer,
        block_type_trees: HuffmanCode => block_type_length_state.block_type_trees,
        block_len_trees: HuffmanCode => block_type_length_state.block_len_trees,
        dist_context_map: u8 => dist_context_map,
        context_map_table: HuffmanCode => context_map_table,
        context_map: u8 => context_map,
        context_modes: u8 => context_modes,
    }
}

impl Record<'_> {
    fn valid(&self) -> bool {
        use BrotliDecoderErrorCode::*;
        use BrotliRunningState::*;

        // Calls return only at input boundaries: no borrowed caller cursor or
        // caller output buffer can appear in the image.
        let fresh = self.state == BROTLI_STATE_UNINITED;
        let Ok(ring) = usize::try_from(self.ringbuffer_size) else {
            return false;
        };
        let Ok(pos) = usize::try_from(self.pos) else {
            return false;
        };
        let window_valid = if fresh {
            self.window_bits == 0
        } else {
            (9..=24).contains(&self.window_bits)
        };
        if !window_valid {
            return false;
        }
        let ring_valid = if ring == 0 {
            self.ringbuffer.is_empty()
                && self.ringbuffer_mask == 0
                && pos == 0
                && self.rb_roundtrips == 0
                && self.partial_pos_out == 0
                && !self.should_wrap_ringbuffer
        } else {
            ring.is_power_of_two()
                && (32..=(1 << self.window_bits)).contains(&ring)
                && self.ringbuffer_mask as usize == ring - 1
                && self.ringbuffer.len() == ring + RING_SLACK
                && pos <= ring + RING_SLACK
                && self
                    .rb_roundtrips
                    .checked_mul(ring as u64)
                    .and_then(|wrapped| wrapped.checked_add(pos as u64))
                    .is_some_and(|total| self.partial_pos_out <= total)
                && (self.rb_roundtrips == 0 || ring == 1 << self.window_bits)
        };
        let block_table_len = if fresh { 0 } else { 3 * HUFFMAN_TABLE };
        ring_valid
            && (self.error_code == BROTLI_DECODER_NEEDS_MORE_INPUT
                || (fresh && self.error_code == BROTLI_DECODER_SUCCESS))
            && self.bit_pos <= 64
            && self.buffer_length <= 8
            && (self.buffer_length == 0 || self.buffered_avail <= self.buffer_length)
            && self.context_mode < 4
            && self.symbol_lists_index == 16
            && self.symbols_lists_array.len() == 720
            && self.mtf.len() == 256
            && self.mtf_upper_bound <= 255
            && self.is_last_metablock <= 1
            && self.is_uncompressed <= 1
            && self.is_metadata <= 1
            && self.size_nibbles <= 7
            && (0..=1 << 24).contains(&self.meta_block_remaining_len)
            && (0..=self.max_backward_distance).contains(&self.max_distance)
            && self.max_backward_distance
                == if fresh {
                    0
                } else {
                    (1 << self.window_bits) - 16
                }
            && self.num_block_types.iter().all(|&n| n <= 256)
            && self.block_length_index < 26
            && self.distance_postfix_bits <= 3
            && self.sub_loop_counter <= 18
            && self.prev_code_len <= 15
            && self.repeat_code_len <= 15
            && self.symbol <= 704
            && self.space <= 32768
            && self.code_length_code_lengths.iter().all(|&len| len <= 5)
            && self.next_symbol[..16]
                .iter()
                .all(|&index| (-16..704).contains(&index))
            && self.distance_postfix_mask == (1 << self.distance_postfix_bits) - 1
            && self.num_direct_distance_codes <= 136
            && self.num_dist_htrees <= 256
            && self.num_literal_htrees <= 256
            && self.max_run_length_prefix <= 16
            && self.context_modes.len() <= 256
            && self.context_modes.iter().all(|&mode| mode < 4)
            && self.context_map.len() <= 256 * 64
            && self.context_map_slice_index <= 255 * 64
            && self.dist_context_map.len() <= 256 * 4
            && self.dist_context_map_slice_index <= 255 * 4
            && self.context_index <= 256 * 64
            && self.block_type_trees.len() == block_table_len
            && valid_codes(&self.block_type_trees)
            && self.block_len_trees.len() == block_table_len
            && valid_codes(&self.block_len_trees)
            && self.context_map_table.len() == HUFFMAN_TABLE
            && valid_codes(&self.context_map_table)
            && valid_codes(&self.table)
            && self.literal_hgroup.valid()
            && self.insert_copy_hgroup.valid()
            && self.distance_hgroup.valid()
            && self.valid_phase(ring)
    }

    fn valid_phase(&self, ring: usize) -> bool {
        use BrotliRunningState::*;

        let [literal, command, distance] = self.num_block_types.map(|n| n as usize);
        let have_block_types = literal != 0 && command != 0 && distance != 0;
        let literal_map_ready = self.context_map.len() == literal * 64;
        let distance_map_ready = self.dist_context_map.len() == distance * 4;
        let have_context_modes = have_block_types && self.context_modes.len() == literal;
        let groups_ready = self.literal_hgroup.htrees.len() == self.num_literal_htrees as usize
            && self.insert_copy_hgroup.htrees.len() == command
            && self.distance_hgroup.htrees.len() == self.num_dist_htrees as usize
            && self.literal_hgroup.alphabet_size == 256
            && self.insert_copy_hgroup.alphabet_size == 704
            && self.num_literal_htrees != 0
            && self.num_dist_htrees != 0;
        match self.state {
            BROTLI_STATE_UNINITED => {
                ring == 0 && self.buffer_length == 0 && self.num_block_types == [0; 3]
            }
            BROTLI_STATE_METABLOCK_HEADER | BROTLI_STATE_METADATA => true,
            BROTLI_STATE_UNCOMPRESSED => ring != 0 && self.is_uncompressed != 0,
            BROTLI_STATE_HUFFMAN_CODE_0
            | BROTLI_STATE_HUFFMAN_CODE_1
            | BROTLI_STATE_HUFFMAN_CODE_2
            | BROTLI_STATE_HUFFMAN_CODE_3 => ring != 0 && (0..3).contains(&self.loop_counter),
            BROTLI_STATE_METABLOCK_HEADER_2 => ring != 0 && have_block_types,
            BROTLI_STATE_CONTEXT_MODES => {
                ring != 0 && have_context_modes && (0..literal as i32).contains(&self.loop_counter)
            }
            BROTLI_STATE_CONTEXT_MAP_1 => ring != 0 && have_context_modes,
            BROTLI_STATE_CONTEXT_MAP_2 => {
                ring != 0
                    && have_context_modes
                    && literal_map_ready
                    && self.num_direct_distance_codes >= 16
            }
            BROTLI_STATE_TREE_GROUP => {
                let group = match self.loop_counter {
                    0 => &self.literal_hgroup,
                    1 => &self.insert_copy_hgroup,
                    2 => &self.distance_hgroup,
                    _ => return false,
                };
                ring != 0
                    && have_context_modes
                    && literal_map_ready
                    && distance_map_ready
                    && groups_ready
                    && self.htree_index >= 0
                    && (self.htree_index as usize) < group.htrees.len()
                    && (self.htree_next_offset as usize)
                        .checked_add(256)
                        .is_some_and(|end| end <= group.codes.len())
            }
            BROTLI_STATE_COMMAND_BEGIN
            | BROTLI_STATE_COMMAND_INNER
            | BROTLI_STATE_COMMAND_POST_DECODE_LITERALS => {
                ring != 0
                    && have_context_modes
                    && literal_map_ready
                    && distance_map_ready
                    && groups_ready
                    && usize::from(self.htree_command_index) < command
                    && u32::from(self.literal_htree_index) < self.num_literal_htrees
                    && u32::from(self.dist_htree_index) < self.num_dist_htrees
                    && self.context_map_slice_index <= self.context_map.len() - 64
                    && self.dist_context_map_slice_index <= self.dist_context_map.len() - 4
                    && self
                        .context_map
                        .iter()
                        .all(|&id| u32::from(id) < self.num_literal_htrees)
                    && self
                        .dist_context_map
                        .iter()
                        .all(|&id| u32::from(id) < self.num_dist_htrees)
            }
            // These phases do not suspend for input.
            _ => false,
        }
    }
}
