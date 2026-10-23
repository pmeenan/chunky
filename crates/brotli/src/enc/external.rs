//! chunky fork addition (D-020, M2): write meta-blocks from externally
//! supplied commands.
//!
//! [`ExternalEncoder`] owns a ring buffer sized from a retained-history cap
//! rather than the declared window (D-017), and writes one actual-length
//! meta-block per call from either caller-supplied [`ExternalCommand`]s or
//! its own q2-class search (the cold path, D-016). Before anything reaches
//! the bit writer, every copy is checked against the retained bytes and every
//! static-dictionary reference is expanded with its transform and compared;
//! a reference that fails is emitted as literals instead (D-017).
//! Dictionary transforms producing fewer than two bytes also become literals
//! to preserve the native writers' minimum-copy-length invariant.
//! Distance short codes and dictionary wire distances are resolved here, at emit time,
//! against the live distance cache and the declared window.
//!
//! Table construction follows `table_quality`: q2 is the fast single-table
//! writer, q4–q9 the greedy block splitter, q10/q11 the full ("oracle")
//! splitter and clustering. Nothing in this module allocates a match hasher
//! unless `search_quality` asks for one.
//!
//! [`ParseLog`] captures the reference encoder's own parse (per meta-block,
//! with each command's distance short code) so it can be re-injected exactly.

#![warn(clippy::all)]

use core::cmp::min;
use std::vec::Vec;

use super::super::alloc::{Allocator, SliceWrapper, SliceWrapperMut};
use super::backward_references::{
    BrotliCreateBackwardReferences, BrotliEncoderParams, UnionHasher,
};
use super::brotli_bit_stream::{
    BrotliWriteEmptyLastMetaBlock, BrotliWritePaddingMetaBlock, RecoderState,
};
use super::combined_alloc::{alloc_default, allocate, BrotliAlloc};
use super::command::{BrotliDistanceParams, Command, ComputeDistanceCode};
use super::encode::{
    BrotliEncoderInitParams, ChooseContextMode, ChooseDistanceParams, EncodeWindowBits,
    HasherReset, InitOrStitchToPreviousBlock, RingBuffer, RingBufferInit, RingBufferWrite,
    SanitizeParams, WrapPosition, WriteMetaBlockInternal,
};
use super::histogram::{CostAccessors, HistogramCommand, HistogramDistance, HistogramLiteral};
use super::input_pair::InputReferenceMut;
use super::interface::{InputPair, PredictionModeContextMap, StaticCommand};
use super::static_dict::BrotliGetDictionary;
use brotli_decompressor::transform::{kNumTransforms, TransformDictionaryWord};

const NUM_SHORT_CODES: usize = 16;
/// The decoder's window gap: a window of `1 << lgwin` reaches back
/// `(1 << lgwin) - 16` bytes.
const WINDOW_GAP: usize = 16;
const INITIAL_DIST_CACHE: [i32; 4] = [4, 11, 15, 16];
const MIN_WINDOW_COPY: u32 = 2;

/// `Reference::Window::short_code` value that asks for an explicit
/// distance code even when a short code would match.
pub const EXPLICIT_DISTANCE: u8 = 16;

/// Where a copy's bytes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reference {
    /// A backward copy `distance` bytes back in retained history.
    /// `short_code` optionally names the distance code to use, for exact
    /// replay of a captured parse: a short code (0..16) is honoured when it
    /// resolves to the same distance, and [`EXPLICIT_DISTANCE`] asks for
    /// the explicit form. Otherwise the cheapest matching code is chosen.
    Window {
        distance: u32,
        short_code: Option<u8>,
    },
    /// A static-dictionary word: `word_idx` within the words of
    /// `word_len` bytes, expanded with `transform` (0..121). The copy
    /// length is the transformed length.
    Dictionary {
        word_len: u8,
        word_idx: u32,
        transform: u8,
    },
}

/// One command in logical form: `insert_len` literals, then `copy_len`
/// bytes from `reference` (no copy when `copy_len` is 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalCommand {
    pub insert_len: u32,
    pub copy_len: u32,
    pub reference: Reference,
}

impl ExternalCommand {
    pub fn literals(n: u32) -> Self {
        Self {
            insert_len: n,
            copy_len: 0,
            reference: Reference::Window {
                distance: 0,
                short_code: None,
            },
        }
    }

    pub fn window(insert_len: u32, copy_len: u32, distance: u32) -> Self {
        Self {
            insert_len,
            copy_len,
            reference: Reference::Window {
                distance,
                short_code: None,
            },
        }
    }

    /// Bytes of input this command covers.
    pub fn len(&self) -> u64 {
        u64::from(self.insert_len) + u64::from(self.copy_len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Encoder options. All of them are part of the determinism contract
/// (D-014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalOptions {
    /// Declared window (WBITS), 10..=24. Bounds only the decoder.
    pub lgwin: u32,
    /// Retained history, 10..=lgwin; see `compact_ring` for the exact reach
    /// ([`ExternalOptions::max_backward`]).
    pub retained_bits: u32,
    /// Largest meta-block: `1 << block_bits` bytes, 10..=24.
    pub block_bits: u32,
    /// Table construction: 2 fast, 3 trivial, 4..=9 greedy, 10..=11 full.
    pub table_quality: u32,
    /// Match search for blocks without supplied commands: 0 for none
    /// (literals only, no hasher allocated), otherwise a quality in 2..=9
    /// whose hasher searches retained history.
    pub search_quality: u32,
    /// Ring buffer layout. Compact: a `1 << retained_bits` ring holds the
    /// open block and the history before it, so copies reach
    /// `(1 << retained_bits) - (1 << block_bits) - 16` bytes back (needs
    /// `block_bits < retained_bits`). Otherwise the reference encoder's
    /// layout: a ring of `2 << max(retained_bits, block_bits)` bytes and a
    /// reach of `(1 << retained_bits) - 16`, for twice the memory.
    pub compact_ring: bool,
}

impl ExternalOptions {
    pub fn new(lgwin: u32, table_quality: u32) -> Self {
        Self {
            lgwin,
            retained_bits: lgwin,
            block_bits: 16,
            table_quality,
            search_quality: 0,
            compact_ring: true,
        }
    }

    /// Farthest back a window copy may reach (before the stream position
    /// limits it).
    pub fn max_backward(&self) -> u64 {
        if self.compact_ring {
            (1u64 << self.retained_bits) - (1u64 << self.block_bits) - WINDOW_GAP as u64
        } else {
            (1u64 << self.retained_bits) - WINDOW_GAP as u64
        }
    }

    /// Ring buffer size in bits (plus a mirrored tail of one block).
    pub fn ring_bits(&self) -> u32 {
        if self.compact_ring {
            self.retained_bits
        } else {
            1 + self.retained_bits.max(self.block_bits)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalError {
    BadOptions(&'static str),
    /// More input than the open block can hold.
    BlockFull,
    /// The stream already ended.
    Finished,
}

/// What one `encode_block` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockReport {
    /// Input offset and length of the meta-block.
    pub start: u64,
    pub len: u32,
    /// Encoded bytes released by this call.
    pub out_bytes: usize,
    pub commands: u32,
    pub literals: u32,
    pub window_copies: u32,
    pub dictionary_copies: u32,
    /// Supplied copies that failed verification or did not fit and were
    /// emitted as literals instead, and their bytes.
    pub rejected_copies: u32,
    pub rejected_bytes: u32,
    /// Block bytes the supplied commands did not cover (emitted as
    /// literals), and supplied bytes beyond the block end (dropped).
    pub uncovered_bytes: u32,
    pub overrun_bytes: u64,
    /// Explicit short-code hints honoured and ignored.
    pub hints_used: u32,
    pub hints_ignored: u32,
    /// Longest window distance emitted (0 if none).
    pub max_distance: u32,
}

/// The distance a short code (0..16) names under `cache`, or `None` if it
/// is not positive.
pub fn short_code_distance(code: usize, cache: &[i32]) -> Option<u32> {
    const DELTA: [i32; 6] = [-1, 1, -2, 2, -3, 3];
    let d = match code {
        0..=3 => cache[code],
        4..=9 => cache[0] + DELTA[code - 4],
        10..=15 => cache[1] + DELTA[code - 10],
        _ => return None,
    };
    if d > 0 {
        Some(d as u32)
    } else {
        None
    }
}

fn push_distance(cache: &mut [i32], distance: u32) {
    cache[3] = cache[2];
    cache[2] = cache[1];
    cache[1] = cache[0];
    cache[0] = distance as i32;
}

/// Expand a static-dictionary word into `dst`; returns its length, or
/// `None` for an out-of-range word or transform.
pub fn expand_dictionary_word(
    word_len: u8,
    word_idx: u32,
    transform: u8,
    dst: &mut [u8; 64],
) -> Option<usize> {
    let dict = BrotliGetDictionary();
    let len = word_len as usize;
    if !(4..=24).contains(&len) || i32::from(transform) >= kNumTransforms {
        return None;
    }
    let nbits = dict.size_bits_by_length[len];
    if nbits == 0 || word_idx >= (1u32 << nbits) {
        return None;
    }
    let off = dict.offsets_by_length[len] as usize + word_idx as usize * len;
    let word = &dict.data[off..off + len];
    let n = TransformDictionaryWord(&mut dst[..], word, len as i32, i32::from(transform));
    Some(n as usize)
}

/// Copy-length code of a command, as the bit writer computes it.
fn copy_len_code(cmd: &Command) -> u32 {
    let modifier = cmd.copy_len_ >> 25;
    let delta = ((modifier | ((modifier & 0x40) << 1)) as u8) as i8 as i32;
    (cmd.copy_len() as i32 + delta) as u32
}

/// Decode native commands back into logical form, simulating the
/// decoder's distance cache from `cache` (at least 4 entries). Distances
/// beyond `min(pos, max_backward)` are static-dictionary references.
fn decode_commands(
    cmds: &[Command],
    dist: &BrotliDistanceParams,
    start: u64,
    max_backward: usize,
    cache: &[i32],
    out: &mut Vec<ExternalCommand>,
) {
    let dict = BrotliGetDictionary();
    let mut cache = [cache[0], cache[1], cache[2], cache[3]];
    let mut pos = start;
    for cmd in cmds {
        let insert_len = cmd.insert_len_;
        pos += u64::from(insert_len);
        let copy_len = cmd.copy_len();
        if copy_len == 0 {
            out.push(ExternalCommand::literals(insert_len));
            continue;
        }
        let code = cmd.restore_distance_code(dist) as usize;
        let distance = if code < NUM_SHORT_CODES {
            short_code_distance(code, &cache).unwrap_or(0)
        } else {
            (code - (NUM_SHORT_CODES - 1)) as u32
        };
        let max_distance = min(pos, max_backward as u64) as u32;
        let reference = if distance > max_distance {
            let word_len = copy_len_code(cmd) as usize;
            let nbits = dict.size_bits_by_length.get(word_len).copied().unwrap_or(0);
            let word_id = distance - max_distance - 1;
            Reference::Dictionary {
                word_len: word_len as u8,
                word_idx: word_id & ((1u32 << nbits) - 1),
                transform: (word_id >> nbits) as u8,
            }
        } else {
            if code != 0 {
                push_distance(&mut cache, distance);
            }
            Reference::Window {
                distance,
                short_code: Some(min(code, NUM_SHORT_CODES) as u8),
            }
        };
        out.push(ExternalCommand {
            insert_len,
            copy_len,
            reference,
        });
        pos += u64::from(copy_len);
    }
}

/// One captured meta-block of a reference encode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedBlock {
    pub start: u64,
    pub len: u32,
    pub commands: Vec<ExternalCommand>,
}

/// Collects the reference encoder's parse as it writes meta-blocks; see
/// `BrotliEncoderStateStruct::enable_parse_log`.
#[derive(Clone, Debug, Default)]
pub struct ParseLog {
    pub blocks: Vec<CapturedBlock>,
}

impl ParseLog {
    pub(crate) fn record(
        &mut self,
        cmds: &[Command],
        params: &BrotliEncoderParams,
        start: u64,
        len: usize,
        saved_dist_cache: &[i32],
    ) {
        let mut commands = Vec::with_capacity(cmds.len());
        let max_backward = (1usize << params.lgwin) - WINDOW_GAP;
        decode_commands(
            cmds,
            &params.dist,
            start,
            max_backward,
            saved_dist_cache,
            &mut commands,
        );
        self.blocks.push(CapturedBlock {
            start,
            len: len as u32,
            commands,
        });
    }
}

/// Scratch for writing one meta-block: the native command array, the bit
/// writer's buffer, histogram scratch and the allocator that block
/// splitting and clustering borrow from. Nothing in it outlives an
/// `encode_block` call, so one workspace per thread serves every stream that
/// thread encodes, one block at a time. Buffers keep their high-water size.
pub struct Workspace<Alloc: BrotliAlloc> {
    m: Alloc,
    commands: <Alloc as Allocator<Command>>::AllocatedMemory,
    storage: <Alloc as Allocator<u8>>::AllocatedMemory,
    literal_scratch: <HistogramLiteral as CostAccessors>::i32vec,
    command_scratch: <HistogramCommand as CostAccessors>::i32vec,
    distance_scratch: <HistogramDistance as CostAccessors>::i32vec,
    logical: Vec<ExternalCommand>,
}

impl<Alloc: BrotliAlloc> Workspace<Alloc> {
    pub fn new(m: Alloc) -> Self {
        Self {
            m,
            commands: alloc_default::<Command, Alloc>(),
            storage: alloc_default::<u8, Alloc>(),
            literal_scratch: HistogramLiteral::make_nnz_storage(),
            command_scratch: HistogramCommand::make_nnz_storage(),
            distance_scratch: HistogramDistance::make_nnz_storage(),
            logical: Vec::new(),
        }
    }

    /// Room for the commands of a `len`-byte block: at most one per two
    /// bytes, plus the final insert.
    fn reserve_commands(&mut self, len: usize) {
        let need = len / 2 + 2;
        if self.commands.slice().len() < need {
            let old = core::mem::take(&mut self.commands);
            <Alloc as Allocator<Command>>::free_cell(&mut self.m, old);
            self.commands = allocate::<Command, _>(&mut self.m, need + len / 4 + 16);
        }
    }

    fn reserve_storage(&mut self, need: usize) {
        if self.storage.slice().len() < need {
            let old = core::mem::take(&mut self.storage);
            <Alloc as Allocator<u8>>::free_cell(&mut self.m, old);
            self.storage = allocate::<u8, _>(&mut self.m, need);
        }
    }
}

/// A per-flush meta-block writer over a capped ring buffer; see the module
/// docs. It owns only per-stream state (the ring, the search hasher and the
/// bit and distance carry); block scratch comes from a [`Workspace`].
pub struct ExternalEncoder<Alloc: BrotliAlloc> {
    m: Alloc,
    opts: ExternalOptions,
    /// Table construction and distance coding (declared window).
    params: BrotliEncoderParams,
    /// Match search (window = the retained cap).
    search_params: BrotliEncoderParams,
    ring: RingBuffer<Alloc>,
    hasher: UnionHasher<Alloc>,
    input_pos: u64,
    block_start: u64,
    dist_cache: [i32; 16],
    saved_dist_cache: [i32; 4],
    last_bytes: u16,
    last_bytes_bits: u8,
    recoder_state: RecoderState,
    /// Bits written since the last byte-aligned flush point.
    dirty: bool,
    finished: bool,
}

impl<Alloc: BrotliAlloc> ExternalEncoder<Alloc> {
    pub fn new(m: Alloc, opts: ExternalOptions) -> Result<Self, ExternalError> {
        if !(10..=24).contains(&opts.lgwin) {
            return Err(ExternalError::BadOptions("lgwin must be 10..=24"));
        }
        if !(10..=opts.lgwin).contains(&opts.retained_bits) {
            return Err(ExternalError::BadOptions(
                "retained_bits must be 10..=lgwin",
            ));
        }
        if !(10..=24).contains(&opts.block_bits) {
            return Err(ExternalError::BadOptions("block_bits must be 10..=24"));
        }
        if opts.compact_ring && opts.block_bits >= opts.retained_bits {
            return Err(ExternalError::BadOptions(
                "a compact ring needs block_bits < retained_bits",
            ));
        }
        if !(2..=11).contains(&opts.table_quality) {
            return Err(ExternalError::BadOptions("table_quality must be 2..=11"));
        }
        if opts.search_quality != 0 && !(2..=9).contains(&opts.search_quality) {
            return Err(ExternalError::BadOptions(
                "search_quality must be 0 or 2..=9",
            ));
        }
        let mut params = BrotliEncoderInitParams();
        params.quality = opts.table_quality as i32;
        params.lgwin = opts.lgwin as i32;
        params.lgblock = opts.block_bits as i32;
        SanitizeParams(&mut params);
        ChooseDistanceParams(&mut params);

        let mut search_params = BrotliEncoderInitParams();
        search_params.quality = opts.search_quality.max(2) as i32;
        search_params.lgwin = opts.retained_bits as i32;
        search_params.lgblock = opts.block_bits as i32;
        search_params.max_backward_distance = opts.max_backward() as usize;
        SanitizeParams(&mut search_params);
        ChooseDistanceParams(&mut search_params);

        // The ring holds the history the cap allows plus the open block,
        // with a mirrored tail of one block so reads of up to a block past
        // any position stay contiguous.
        let mut ring = RingBufferInit();
        let ring_bits = opts.ring_bits();
        ring.size_ = 1u32 << ring_bits;
        ring.mask_ = ring.size_ - 1;
        ring.tail_size_ = 1u32 << opts.block_bits;
        ring.total_size_ = ring.size_ + ring.tail_size_;

        let mut last_bytes = 0u16;
        let mut last_bytes_bits = 0u8;
        EncodeWindowBits(params.lgwin, false, &mut last_bytes, &mut last_bytes_bits);
        let cache = INITIAL_DIST_CACHE;
        let mut dist_cache = [0i32; 16];
        dist_cache[..4].copy_from_slice(&cache);
        Ok(Self {
            m,
            opts,
            params,
            search_params,
            ring,
            hasher: UnionHasher::default(),
            input_pos: 0,
            block_start: 0,
            dist_cache,
            saved_dist_cache: cache,
            last_bytes,
            last_bytes_bits,
            recoder_state: RecoderState::new(),
            dirty: false,
            finished: false,
        })
    }

    pub fn options(&self) -> &ExternalOptions {
        &self.opts
    }

    /// Start a new stream with the same options, keeping allocations: a
    /// full-size ring and the hasher are reused in place (the hasher is
    /// cleared on its next use). A ring still in its small first-write
    /// buffer is released, since `RingBufferWrite` would regrow it from
    /// scratch anyway. Output after a reset matches a fresh encoder's.
    pub fn reset(&mut self) {
        if self.ring.cur_size_ < self.ring.total_size_ {
            let old = core::mem::take(&mut self.ring.data_mo);
            <Alloc as Allocator<u8>>::free_cell(&mut self.m, old);
            self.ring.cur_size_ = 0;
        } else {
            // A fresh buffer has zeros in the two bytes before the data and
            // in the ring's last two bytes, which writes copy into them.
            let (bi, size) = (self.ring.buffer_index, self.ring.size_ as usize);
            let d = self.ring.data_mo.slice_mut();
            for i in [bi - 2, bi - 1, bi + size - 2, bi + size - 1] {
                d[i] = 0;
            }
        }
        self.ring.pos_ = 0;
        HasherReset(&mut self.hasher);
        self.input_pos = 0;
        self.block_start = 0;
        self.dist_cache = [0; 16];
        self.dist_cache[..4].copy_from_slice(&INITIAL_DIST_CACHE);
        self.saved_dist_cache = INITIAL_DIST_CACHE;
        EncodeWindowBits(
            self.params.lgwin,
            false,
            &mut self.last_bytes,
            &mut self.last_bytes_bits,
        );
        self.recoder_state = RecoderState::new();
        self.dirty = false;
        self.finished = false;
    }

    /// Bytes appended so far.
    pub fn input_pos(&self) -> u64 {
        self.input_pos
    }

    /// Bytes appended since the last meta-block.
    pub fn pending(&self) -> usize {
        (self.input_pos - self.block_start) as usize
    }

    /// Room left in the open block.
    pub fn block_room(&self) -> usize {
        (1usize << self.opts.block_bits) - self.pending()
    }

    /// Farthest back a window copy at absolute position `pos` may reach.
    pub fn max_copy_distance(&self, pos: u64) -> u32 {
        min(pos, self.opts.max_backward()) as u32
    }

    /// The live distance cache (last four distances, newest first).
    pub fn distance_cache(&self) -> [i32; 4] {
        [
            self.dist_cache[0],
            self.dist_cache[1],
            self.dist_cache[2],
            self.dist_cache[3],
        ]
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The retained byte at absolute position `pos`, if still in the ring.
    pub fn history_byte(&self, pos: u64) -> Option<u8> {
        let reach = self.opts.max_backward() + (1u64 << self.opts.block_bits);
        if pos >= self.input_pos || self.input_pos - pos > reach {
            return None;
        }
        let i = self.ring.buffer_index + (pos as usize & self.ring.mask_ as usize);
        Some(self.ring.data_mo.slice()[i])
    }

    /// Add input to the open block.
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), ExternalError> {
        if self.finished {
            return Err(ExternalError::Finished);
        }
        if bytes.len() > self.block_room() {
            return Err(ExternalError::BlockFull);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        RingBufferWrite(&mut self.m, bytes, bytes.len(), &mut self.ring);
        self.input_pos += bytes.len() as u64;
        // As in `copy_input_to_ring_buffer`: zero the slack after the data
        // so hashing past the end reads deterministic bytes.
        if self.ring.pos_ <= self.ring.mask_ {
            let start = self.ring.buffer_index + self.ring.pos_ as usize;
            for b in self.ring.data_mo.slice_mut()[start..start + 7].iter_mut() {
                *b = 0;
            }
        }
        Ok(())
    }

    /// Close the open block as one meta-block (nothing for an empty block
    /// unless `is_last`), appending the completed bytes to `out`. With
    /// `commands`, they are the block's parse (verified and repaired, never
    /// trusted); without, the encoder searches if `search_quality` is set
    /// and emits literals otherwise. Bits of a final partial byte stay
    /// pending until `flush` or the last block.
    pub fn encode_block(
        &mut self,
        ws: &mut Workspace<Alloc>,
        commands: Option<&[ExternalCommand]>,
        is_last: bool,
        out: &mut Vec<u8>,
    ) -> Result<BlockReport, ExternalError> {
        if self.finished {
            return Err(ExternalError::Finished);
        }
        let len = self.pending();
        let mut report = BlockReport {
            start: self.block_start,
            len: len as u32,
            ..BlockReport::default()
        };
        if len == 0 {
            if is_last {
                report.out_bytes = self.write_empty_last(out);
            }
            return Ok(report);
        }
        let mut logical = core::mem::take(&mut ws.logical);
        logical.clear();
        ws.reserve_commands(len);
        match commands {
            Some(c) => logical.extend_from_slice(c),
            None if self.opts.search_quality != 0 => self.search(ws, len, &mut logical),
            None => logical.push(ExternalCommand::literals(len as u32)),
        }
        let (num_commands, num_literals) =
            self.verify_and_resolve(ws.commands.slice_mut(), &logical, &mut report);
        ws.logical = logical;
        report.commands = num_commands as u32;
        report.literals = num_literals as u32;

        let mask = self.ring.mask_ as usize;
        let data = &self.ring.data_mo.slice()[self.ring.buffer_index..];
        let literal_context_mode = ChooseContextMode(
            &self.params,
            data,
            WrapPosition(self.block_start) as usize,
            mask,
            len,
        );
        let prev_byte = if self.block_start > 0 {
            data[(self.block_start - 1) as usize & mask]
        } else {
            0
        };
        let prev_byte2 = if self.block_start > 1 {
            data[(self.block_start - 2) as usize & mask]
        } else {
            0
        };
        ws.reserve_storage(2 * len + 503 + 24 + 2);
        let storage = ws.storage.slice_mut();
        storage[0] = self.last_bytes as u8;
        storage[1] = (self.last_bytes >> 8) as u8;
        let mut storage_ix = usize::from(self.last_bytes_bits);
        let mut nop = |_: &mut PredictionModeContextMap<InputReferenceMut>,
                       _: &mut [StaticCommand],
                       _: InputPair,
                       _: &mut Alloc| ();
        WriteMetaBlockInternal(
            &mut ws.m,
            &self.ring.data_mo.slice()[self.ring.buffer_index..],
            mask,
            self.block_start,
            len,
            is_last,
            literal_context_mode,
            &self.params,
            &mut ws.literal_scratch,
            &mut ws.command_scratch,
            &mut ws.distance_scratch,
            prev_byte,
            prev_byte2,
            num_literals,
            num_commands,
            ws.commands.slice_mut(),
            &self.saved_dist_cache,
            &mut self.dist_cache,
            &mut self.recoder_state,
            &mut storage_ix,
            ws.storage.slice_mut(),
            &mut nop,
        );
        self.block_start = self.input_pos;
        self.saved_dist_cache.copy_from_slice(&self.dist_cache[..4]);
        report.out_bytes = self.release(ws.storage.slice(), storage_ix, is_last, out);
        self.dirty = !is_last;
        self.finished = is_last;
        Ok(report)
    }

    /// Byte-align the output so every byte written so far can be delivered
    /// (an empty metadata block, RFC 7932 §9.2), appending to `out`.
    /// A no-op when nothing was written since the last alignment. The open
    /// block must be empty; close it with `encode_block` first.
    pub fn flush(&mut self, out: &mut Vec<u8>) -> Result<usize, ExternalError> {
        if self.finished || !self.dirty {
            return Ok(0);
        }
        if self.pending() != 0 {
            return Err(ExternalError::BlockFull);
        }
        let mut storage = [0u8; 16];
        storage[0] = self.last_bytes as u8;
        storage[1] = (self.last_bytes >> 8) as u8;
        let mut ix = usize::from(self.last_bytes_bits);
        BrotliWritePaddingMetaBlock(&mut ix, &mut storage);
        self.dirty = false;
        Ok(self.release(&storage, ix, false, out))
    }

    /// End the stream: close the open block as the last meta-block, or
    /// write an empty last meta-block.
    pub fn finish(
        &mut self,
        ws: &mut Workspace<Alloc>,
        out: &mut Vec<u8>,
    ) -> Result<usize, ExternalError> {
        if self.finished {
            return Ok(0);
        }
        Ok(self.encode_block(ws, None, true, out)?.out_bytes)
    }

    fn write_empty_last(&mut self, out: &mut Vec<u8>) -> usize {
        let mut storage = [0u8; 16];
        storage[0] = self.last_bytes as u8;
        storage[1] = (self.last_bytes >> 8) as u8;
        let mut ix = usize::from(self.last_bytes_bits);
        BrotliWriteEmptyLastMetaBlock(&mut ix, &mut storage);
        self.finished = true;
        self.dirty = false;
        self.release(&storage, ix, true, out)
    }

    /// Append the whole bytes of `storage[..ix bits]` to `out` and keep the
    /// partial byte as carry (or pad and append it when `last`).
    fn release(&mut self, storage: &[u8], ix: usize, last: bool, out: &mut Vec<u8>) -> usize {
        let whole = if last { (ix + 7) >> 3 } else { ix >> 3 };
        out.extend_from_slice(&storage[..whole]);
        if last {
            self.last_bytes = 0;
            self.last_bytes_bits = 0;
        } else {
            self.last_bytes = u16::from(storage[whole]) | (u16::from(storage[whole + 1]) << 8);
            self.last_bytes_bits = (ix & 7) as u8;
        }
        whole
    }

    /// q2-class search over the open block with the persistent hasher,
    /// limited to retained history.
    fn search(
        &mut self,
        ws: &mut Workspace<Alloc>,
        len: usize,
        logical: &mut Vec<ExternalCommand>,
    ) {
        let mask = self.ring.mask_ as usize;
        let position = WrapPosition(self.block_start) as usize;
        let prev_end = WrapPosition(self.block_start.saturating_sub(1));
        // Positions wrap at 1 GiB (as in the reference encoder); stale
        // hash entries must not survive the wrap.
        if self.block_start > 0 && (position as u32) < prev_end {
            HasherReset(&mut self.hasher);
        }
        InitOrStitchToPreviousBlock(
            &mut self.m,
            &mut self.hasher,
            &self.ring.data_mo.slice()[self.ring.buffer_index..],
            mask,
            None,
            &mut self.search_params,
            position,
            len,
            false,
        );
        let mut cache = self.dist_cache;
        let start_cache = cache;
        let mut last_insert_len = 0usize;
        let mut num_commands = 0usize;
        let mut num_literals = 0usize;
        BrotliCreateBackwardReferences(
            &mut self.m,
            BrotliGetDictionary(),
            len,
            position,
            &self.ring.data_mo.slice()[self.ring.buffer_index..],
            mask,
            None,
            &self.search_params,
            &mut self.hasher,
            &mut cache,
            &mut last_insert_len,
            ws.commands.slice_mut(),
            &mut num_commands,
            &mut num_literals,
        );
        let max_backward = self.opts.max_backward() as usize;
        decode_commands(
            &ws.commands.slice()[..num_commands],
            &self.search_params.dist,
            self.block_start,
            max_backward,
            &start_cache,
            logical,
        );
        if last_insert_len > 0 {
            logical.push(ExternalCommand::literals(last_insert_len as u32));
        }
    }

    /// Check every copy against retained history (or the dictionary),
    /// demote failures to literals, clip to the block, resolve distance
    /// codes against the live cache and build native commands. Returns
    /// (commands, literals).
    fn verify_and_resolve(
        &mut self,
        commands: &mut [Command],
        logical: &[ExternalCommand],
        report: &mut BlockReport,
    ) -> (usize, usize) {
        let start = self.block_start;
        let end = self.input_pos;
        let mask = self.ring.mask_ as usize;
        let bi = self.ring.buffer_index;
        let window_limit = (1u64 << self.opts.lgwin) - WINDOW_GAP as u64;
        let retained_limit = self.opts.max_backward();
        let dict = BrotliGetDictionary();
        let mut cache = self.dist_cache;
        let mut n = 0usize;
        let mut num_literals = 0usize;
        let mut insert = 0usize;
        let mut pos = start;
        let mut word = [0u8; 64];
        let mut iter = logical.iter();
        while pos < end {
            let c = match iter.next() {
                Some(c) => *c,
                None => break,
            };
            let ins = min(u64::from(c.insert_len), end - pos);
            insert += ins as usize;
            pos += ins;
            if pos == end {
                // The block ends inside or right after this insert: the rest
                // of the command lies beyond it (overrun, not a rejected copy).
                report.overrun_bytes += u64::from(c.insert_len) - ins + u64::from(c.copy_len);
                break;
            }
            if c.copy_len == 0 {
                continue;
            }
            let room = end - pos;
            let clen = min(u64::from(c.copy_len), room) as u32;
            report.overrun_bytes += u64::from(c.copy_len - clen);
            let data = &self.ring.data_mo.slice()[bi..];
            let at = pos as usize & mask;
            // (copy length code, distance code), when the copy verifies.
            let resolved: Option<(u32, usize)> = match c.reference {
                Reference::Window {
                    distance,
                    short_code,
                } => {
                    let max_d = min(pos, retained_limit);
                    if clen >= MIN_WINDOW_COPY && distance >= 1 && u64::from(distance) <= max_d && {
                        // The ring's mirrored tail keeps both ranges
                        // contiguous: clen <= one block <= tail size.
                        let from = (pos - u64::from(distance)) as usize & mask;
                        data[from..from + clen as usize] == data[at..at + clen as usize]
                    } {
                        let mut code = None;
                        if let Some(h) = short_code {
                            if h == EXPLICIT_DISTANCE {
                                code = Some(distance as usize + NUM_SHORT_CODES - 1);
                                report.hints_used += 1;
                            } else if short_code_distance(h as usize, &cache) == Some(distance) {
                                code = Some(h as usize);
                                report.hints_used += 1;
                            } else {
                                report.hints_ignored += 1;
                            }
                        }
                        let code = code.unwrap_or_else(|| {
                            ComputeDistanceCode(distance as usize, distance as usize, &cache)
                        });
                        if code != 0 {
                            push_distance(&mut cache, distance);
                        }
                        report.window_copies += 1;
                        report.max_distance = report.max_distance.max(distance);
                        Some((clen, code))
                    } else {
                        None
                    }
                }
                Reference::Dictionary {
                    word_len,
                    word_idx,
                    transform,
                } => match expand_dictionary_word(word_len, word_idx, transform, &mut word) {
                    Some(wlen)
                        if wlen as u32 == clen
                            && clen == c.copy_len
                            // The native writers assume every copy emits at
                            // least two bytes (scratch bounds and preceding
                            // literal contexts). Short dictionary transforms
                            // are legal br but must be emitted as literals.
                            && clen >= MIN_WINDOW_COPY
                            && word[..wlen] == data[at..at + wlen] =>
                    {
                        let nbits = dict.size_bits_by_length[word_len as usize];
                        let wire = min(pos, window_limit)
                            + 1
                            + ((u64::from(transform) << nbits) | u64::from(word_idx));
                        if wire <= self.params.dist.max_distance as u64 {
                            report.dictionary_copies += 1;
                            Some((u32::from(word_len), wire as usize + NUM_SHORT_CODES - 1))
                        } else {
                            None
                        }
                    }
                    _ => None,
                },
            };
            match resolved {
                Some((copy_code, dist_code)) => {
                    commands[n].init(
                        &self.params.dist,
                        insert,
                        clen as usize,
                        copy_code as usize,
                        dist_code,
                    );
                    n += 1;
                    num_literals += insert;
                    insert = 0;
                }
                None => {
                    report.rejected_copies += 1;
                    report.rejected_bytes += clen;
                    insert += clen as usize;
                }
            }
            pos += u64::from(clen);
        }
        for c in iter {
            report.overrun_bytes += c.len();
        }
        if pos < end {
            report.uncovered_bytes += (end - pos) as u32;
            insert += (end - pos) as usize;
        }
        if insert > 0 {
            commands[n].init_insert(insert);
            n += 1;
            num_literals += insert;
        }
        self.dist_cache[..4].copy_from_slice(&cache[..4]);
        (n, num_literals)
    }
}
