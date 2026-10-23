//! The M2 inspector: an independent, instrumented RFC 7932 (`br`) decoder.
//!
//! [`inspect`] decodes a complete brotli stream and reports its meta-block
//! structure: where every meta-block starts and ends in the stream (in bits),
//! which bytes of the output it produces, how many bits its header (prefix
//! codes, context maps) costs, and how the rest of its bits split between
//! literals, insert-and-copy commands, distances and block switches.
//!
//! It is written from the RFC, not on top of the encoder fork or either
//! reference decoder, so it doubles as a third independent decoder for
//! round-trip verification. Only the static dictionary's byte table and its
//! per-length offset / size-bit tables come from `brotli-decompressor`; the
//! transform table (RFC 7932 Appendix B) and context lookup tables (§7.1) are
//! transcribed here and cross-checked against other implementations in tests.
//!
//! It accepts exactly the streams the C reference decoder accepts, as far as
//! the tests can tell: padding bits must be zero, prefix codes must be
//! complete, nothing may follow the last meta-block, and large-window streams
//! are rejected. Malformed or truncated input returns an [`InspectError`]
//! carrying the bit offset where decoding failed; it never panics. Output is
//! grown only as bytes are produced and is capped (see [`inspect_with_limit`]),
//! so a hostile MLEN cannot force a large allocation.

use core::fmt;

use brotli_decompressor::dictionary::{
    kBrotliDictionary, kBrotliDictionaryOffsetsByLength, kBrotliDictionarySizeBitsByLength,
};

/// Output cap used by [`inspect`]: 256 MiB of decoded bytes.
pub const DEFAULT_MAX_OUTPUT: usize = 256 << 20;

/// What a meta-block is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Prefix-coded commands and literals.
    Compressed,
    /// `ISUNCOMPRESSED`: raw bytes after byte alignment.
    Uncompressed,
    /// `MNIBBLES = 0`: no output; `MSKIPLEN` bytes of metadata skipped. An
    /// encoder flush's byte-alignment block is metadata with no skip bytes.
    Metadata,
    /// `ISLAST` with `ISLASTEMPTY`: ends the stream, no output.
    EmptyLast,
}

impl BlockKind {
    /// Short lowercase name for tables and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Compressed => "compressed",
            BlockKind::Uncompressed => "uncompressed",
            BlockKind::Metadata => "metadata",
            BlockKind::EmptyLast => "empty-last",
        }
    }
}

/// One meta-block. Bit positions count from the first bit of the stream
/// (LSB of byte 0). For every block:
///
/// `header_bits + pad_bits + literal_bits + command_bits + distance_bits +
/// block_switch_bits + 8 * metadata_len == end_bit - start_bit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaBlock {
    pub kind: BlockKind,
    pub is_last: bool,
    /// First bit of the meta-block header.
    pub start_bit: u64,
    /// One past the block's last bit. Byte-alignment padding before
    /// uncompressed or metadata bytes, and the stream's final padding after
    /// the last block, belong to the block.
    pub end_bit: u64,
    /// Offset of the block's first output byte in the decoded stream.
    pub out_start: u64,
    /// Bytes of output the block produces (`MLEN`; 0 for metadata and empty
    /// last blocks).
    pub mlen: u64,
    /// Bits before the first command: the meta-block header and, for
    /// compressed blocks, block-type/count codes and initial counts,
    /// NPOSTFIX/NDIRECT, context modes, context maps and all prefix codes.
    /// Excludes alignment padding (see `pad_bits`).
    pub header_bits: u64,
    /// Zero bits skipped for byte alignment inside this block.
    pub pad_bits: u64,
    /// `MSKIPLEN` for metadata blocks; 0 otherwise.
    pub metadata_len: u64,
    /// NBLTYPES for literals, insert-and-copy commands and distances.
    pub nbltypes: [u32; 3],
    pub npostfix: u32,
    pub ndirect: u32,
    /// Context mode per literal block type (0 LSB6, 1 MSB6, 2 UTF8, 3 signed).
    pub context_modes: Vec<u8>,
    pub ntrees_lit: u32,
    pub ntrees_dist: u32,
    /// Insert-and-copy commands decoded.
    pub commands: u64,
    /// Literal bytes (raw bytes for uncompressed blocks).
    pub literals: u64,
    /// Backward copies (commands whose copy is not a dictionary reference).
    pub copies: u64,
    /// Bytes produced by backward copies.
    pub copy_bytes: u64,
    /// Static-dictionary references.
    pub dict_refs: u64,
    /// Bytes produced by dictionary references (after transforms).
    pub dict_bytes: u64,
    /// Copies and dictionary references whose distance came from distance
    /// codes 0..15 (the last-distance ring), including the implicit code 0
    /// of command codes below 128.
    pub last_distance_codes: u64,
    /// Block switches decoded inside the command loop, per category.
    pub block_switches: [u64; 3],
    /// Bits of literal prefix codes (all bits of uncompressed bytes).
    pub literal_bits: u64,
    /// Bits of insert-and-copy symbols plus their insert/copy extra bits.
    pub command_bits: u64,
    /// Bits of distance symbols plus their extra bits.
    pub distance_bits: u64,
    /// Bits of block switches (type code, count code, count extra bits)
    /// inside the command loop.
    pub block_switch_bits: u64,
}

impl MetaBlock {
    fn new(start_bit: u64, out_start: u64) -> Self {
        Self {
            kind: BlockKind::Compressed,
            is_last: false,
            start_bit,
            end_bit: start_bit,
            out_start,
            mlen: 0,
            header_bits: 0,
            pad_bits: 0,
            metadata_len: 0,
            nbltypes: [0; 3],
            npostfix: 0,
            ndirect: 0,
            context_modes: Vec::new(),
            ntrees_lit: 0,
            ntrees_dist: 0,
            commands: 0,
            literals: 0,
            copies: 0,
            copy_bytes: 0,
            dict_refs: 0,
            dict_bytes: 0,
            last_distance_codes: 0,
            block_switches: [0; 3],
            literal_bits: 0,
            command_bits: 0,
            distance_bits: 0,
            block_switch_bits: 0,
        }
    }
}

/// A decoded stream and its structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    /// Window bits from the stream header (10..=24).
    pub wbits: u32,
    /// Bits of the WBITS stream header (1, 4 or 7); the first meta-block
    /// starts here.
    pub stream_header_bits: u64,
    /// The decoded body.
    pub decoded: Vec<u8>,
    /// Meta-blocks in stream order; contiguous in bits.
    pub blocks: Vec<MetaBlock>,
    /// Stream length in bits (`8 * stream.len()`); the last block ends here.
    pub total_bits: u64,
}

/// Why and where decoding failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectError {
    /// Bit offset in the stream at which the problem was detected.
    pub bit: u64,
    pub reason: &'static str,
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at bit {} (byte {})",
            self.reason,
            self.bit,
            self.bit / 8
        )
    }
}

impl std::error::Error for InspectError {}

/// Decode and inspect a complete brotli stream, with output capped at
/// [`DEFAULT_MAX_OUTPUT`].
pub fn inspect(stream: &[u8]) -> Result<Inspection, InspectError> {
    inspect_with_limit(stream, DEFAULT_MAX_OUTPUT)
}

/// Decode and inspect a complete brotli stream, failing once the decoded
/// output would exceed `max_output` bytes.
pub fn inspect_with_limit(stream: &[u8], max_output: usize) -> Result<Inspection, InspectError> {
    let mut d = Decoder {
        br: Bits::new(stream),
        out: Vec::new(),
        max_out: max_output,
        max_backward: 0,
        dist: [4, 11, 15, 16],
    };
    let wbits = d.read_wbits()?;
    d.max_backward = (1u64 << wbits) - 16;
    let stream_header_bits = d.br.pos;
    let mut blocks = Vec::new();
    loop {
        let mb = d.meta_block()?;
        let last = mb.is_last;
        blocks.push(mb);
        if last {
            break;
        }
    }
    if d.br.pos != d.br.end {
        return Err(d.br.err("data after the last meta-block"));
    }
    Ok(Inspection {
        wbits,
        stream_header_bits,
        decoded: d.out,
        blocks,
        total_bits: d.br.end,
    })
}

// ---------------------------------------------------------------------------
// Bit reader (RFC 7932 §2: LSB-first within bytes).

struct Bits<'a> {
    data: &'a [u8],
    pos: u64,
    end: u64,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: data.len() as u64 * 8,
        }
    }

    fn err(&self, reason: &'static str) -> InspectError {
        InspectError {
            bit: self.pos,
            reason,
        }
    }

    /// The next `n` (<= 24) bits without consuming them; zero past the end.
    fn peek(&self, n: u32) -> u32 {
        let byte = (self.pos >> 3) as usize;
        let mut v = 0u64;
        for i in 0..4 {
            if let Some(&b) = self.data.get(byte + i) {
                v |= u64::from(b) << (8 * i);
            }
        }
        ((v >> (self.pos & 7)) & ((1u64 << n) - 1)) as u32
    }

    fn skip(&mut self, n: u32) -> Result<(), InspectError> {
        if self.end - self.pos < u64::from(n) {
            return Err(self.err("truncated stream"));
        }
        self.pos += u64::from(n);
        Ok(())
    }

    fn read(&mut self, n: u32) -> Result<u32, InspectError> {
        let v = self.peek(n);
        self.skip(n)?;
        Ok(v)
    }

    /// Skip to the next byte boundary; the skipped bits must be zero.
    /// Returns the number of bits skipped.
    fn align(&mut self) -> Result<u64, InspectError> {
        let at = self.pos;
        let pad = ((8 - (self.pos & 7)) & 7) as u32;
        if self.read(pad)? != 0 {
            return Err(InspectError {
                bit: at,
                reason: "non-zero padding bits",
            });
        }
        Ok(u64::from(pad))
    }
}

// ---------------------------------------------------------------------------
// Prefix codes (RFC 7932 §3).

const ROOT_BITS: u32 = 8;

/// A canonical prefix code: an 8-bit root lookup table for short codes and a
/// canonical bit-by-bit walk for longer ones.
struct Prefix {
    /// A one-symbol code: the symbol, decoded from zero bits.
    single: Option<u16>,
    /// Indexed by the next 8 stream bits: `symbol | len << 16`, or 0 when the
    /// code is longer than 8 bits.
    table: Vec<u32>,
    /// Number of codes of each length.
    counts: [u16; 16],
    /// Used symbols ordered by (length, symbol).
    sorted: Vec<u16>,
}

impl Prefix {
    fn single(sym: u16) -> Self {
        Self {
            single: Some(sym),
            table: Vec::new(),
            counts: [0; 16],
            sorted: Vec::new(),
        }
    }

    /// Build from code lengths (0 = unused, at most 15). The caller has
    /// checked the code is complete or has exactly one used symbol; anything
    /// else still decodes safely, failing on unassigned codes.
    fn from_lengths(lengths: &[u8]) -> Self {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[usize::from(l & 15)] += 1;
        }
        counts[0] = 0;
        let used: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if used == 1 {
            let sym = lengths.iter().position(|&l| l != 0).unwrap_or(0);
            return Self::single(sym as u16);
        }
        let mut offs = [0usize; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + usize::from(counts[l]);
        }
        let mut sorted = vec![0u16; used];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let l = usize::from(l & 15);
                sorted[offs[l]] = sym as u16;
                offs[l] += 1;
            }
        }
        let mut table = vec![0u32; 1 << ROOT_BITS];
        let mut code = 0u32;
        let mut k = 0;
        for len in 1..=ROOT_BITS {
            for _ in 0..counts[len as usize] {
                // Codes are sent MSB first, so the table index is the code
                // reversed.
                let rev = code.reverse_bits() >> (32 - len);
                let entry = u32::from(sorted[k]) | len << 16;
                let mut i = rev as usize;
                while i < table.len() {
                    table[i] = entry;
                    i += 1 << len;
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Self {
            single: None,
            table,
            counts,
            sorted,
        }
    }

    fn decode(&self, br: &mut Bits<'_>) -> Result<u16, InspectError> {
        if let Some(s) = self.single {
            return Ok(s);
        }
        let e = self.table[br.peek(ROOT_BITS) as usize];
        let len = e >> 16;
        if len != 0 {
            br.skip(len)?;
            return Ok(e as u16);
        }
        // Longer than the root table: canonical walk, one bit at a time.
        let at = br.pos;
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.read(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return self
                    .sorted
                    .get((index + code - first) as usize)
                    .copied()
                    .ok_or(InspectError {
                        bit: at,
                        reason: "invalid prefix code",
                    });
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InspectError {
            bit: at,
            reason: "invalid prefix code",
        })
    }
}

/// Order in which code length code lengths are sent (§3.5).
const CL_ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Read one code length code length (0..=5) with the fixed §3.5 code.
fn read_cl_len(br: &mut Bits<'_>) -> Result<u8, InspectError> {
    let v = br.peek(4);
    let (sym, n) = match v & 3 {
        0 => (0, 2),
        2 => (3, 2),
        1 => (4, 2),
        _ if v & 4 == 0 => (2, 3),
        _ if v & 8 == 0 => (1, 4),
        _ => (5, 4),
    };
    br.skip(n)?;
    Ok(sym)
}

/// Read a prefix code over `alphabet` symbols (§3.4 simple, §3.5 complex).
fn read_prefix(br: &mut Bits<'_>, alphabet: usize) -> Result<Prefix, InspectError> {
    let hskip = br.read(2)?;
    let mut lengths = vec![0u8; alphabet];
    if hskip == 1 {
        let nsym = br.read(2)? as usize + 1;
        let bits = usize::BITS - alphabet.saturating_sub(1).leading_zeros();
        let mut syms = [0usize; 4];
        for i in 0..nsym {
            let at = br.pos;
            let s = br.read(bits)? as usize;
            if s >= alphabet {
                return Err(InspectError {
                    bit: at,
                    reason: "simple prefix code symbol out of range",
                });
            }
            if syms[..i].contains(&s) {
                return Err(InspectError {
                    bit: at,
                    reason: "simple prefix code repeats a symbol",
                });
            }
            syms[i] = s;
        }
        let lens: &[u8] = match nsym {
            1 => return Ok(Prefix::single(syms[0] as u16)),
            2 => &[1, 1],
            3 => &[1, 2, 2],
            _ => {
                if br.read(1)? == 0 {
                    &[2, 2, 2, 2]
                } else {
                    &[1, 2, 3, 3]
                }
            }
        };
        for (&s, &l) in syms.iter().zip(lens) {
            lengths[s] = l;
        }
        return Ok(Prefix::from_lengths(&lengths));
    }

    // Complex code: first the code length code.
    let at = br.pos;
    let mut cl = [0u8; 18];
    let mut space = 32i32;
    let mut num = 0;
    for &idx in &CL_ORDER[hskip as usize..] {
        let v = read_cl_len(br)?;
        cl[idx] = v;
        if v != 0 {
            space -= 32 >> v;
            num += 1;
            if space <= 0 {
                break;
            }
        }
    }
    if !(num == 1 || space == 0) {
        return Err(InspectError {
            bit: at,
            reason: "code length code is not complete",
        });
    }
    let clcode = Prefix::from_lengths(&cl);

    // Then the symbol code lengths, with repeat codes 16 and 17.
    let at = br.pos;
    let mut space = 32768i32;
    let mut prev = 8u8;
    let mut repeat = 0u32;
    let mut repeat_len = 0u8;
    let mut sym = 0usize;
    while sym < alphabet && space > 0 {
        let p = clcode.decode(br)?;
        if p < 16 {
            repeat = 0;
            lengths[sym] = p as u8;
            sym += 1;
            if p != 0 {
                prev = p as u8;
                space -= 32768 >> p;
            }
        } else {
            let (extra, new_len) = if p == 16 { (2, prev) } else { (3, 0) };
            if repeat_len != new_len {
                repeat = 0;
                repeat_len = new_len;
            }
            let old = repeat;
            if repeat > 0 {
                repeat = (repeat - 2) << extra;
            }
            repeat += br.read(extra)? + 3;
            // repeat >= old: either old == 0, or (old - 2) * 4 + 3 >= old.
            let delta = (repeat - old) as usize;
            if delta > alphabet - sym {
                return Err(br.err("code length repeat overflows the alphabet"));
            }
            lengths[sym..sym + delta].fill(repeat_len);
            sym += delta;
            if repeat_len != 0 {
                space -= (delta as i32) << (15 - repeat_len);
            }
        }
    }
    if space != 0 {
        return Err(InspectError {
            bit: at,
            reason: "prefix code is not complete",
        });
    }
    Ok(Prefix::from_lengths(&lengths))
}

// ---------------------------------------------------------------------------
// Header fields, block switching and context maps (§6, §7, §9).

/// VarLenUint8 (§9.2): 0..=255.
fn read_varlen8(br: &mut Bits<'_>) -> Result<u32, InspectError> {
    if br.read(1)? == 0 {
        return Ok(0);
    }
    let n = br.read(3)?;
    if n == 0 {
        Ok(1)
    } else {
        Ok((1 << n) + br.read(n)?)
    }
}

/// Block count codes (§6): (offset, extra bits).
#[rustfmt::skip]
const BLOCK_COUNT: [(u32, u32); 26] = [
    (1, 2), (5, 2), (9, 2), (13, 2), (17, 3), (25, 3),
    (33, 3), (41, 3), (49, 4), (65, 4), (81, 4), (97, 4),
    (113, 5), (145, 5), (177, 5), (209, 5), (241, 6), (305, 6),
    (369, 7), (497, 8), (753, 9), (1265, 10), (2289, 11), (4337, 12),
    (8433, 13), (16625, 24),
];

fn read_block_count(br: &mut Bits<'_>, code: &Prefix) -> Result<u32, InspectError> {
    let (off, bits) = BLOCK_COUNT[usize::from(code.decode(br)?)];
    Ok(off + br.read(bits)?)
}

/// Block type and remaining count for one category (§6).
struct BlockSwitch {
    n: u32,
    codes: Option<(Prefix, Prefix)>,
    cur: u32,
    prev: u32,
    left: u32,
}

impl BlockSwitch {
    fn read(br: &mut Bits<'_>, n: u32) -> Result<Self, InspectError> {
        if n < 2 {
            // One block type: its count never runs out within a meta-block
            // (MLEN <= 2^24).
            return Ok(Self {
                n,
                codes: None,
                cur: 0,
                prev: 1,
                left: u32::MAX,
            });
        }
        let types = read_prefix(br, n as usize + 2)?;
        let counts = read_prefix(br, 26)?;
        let left = read_block_count(br, &counts)?;
        Ok(Self {
            n,
            codes: Some((types, counts)),
            cur: 0,
            prev: 1,
            left,
        })
    }

    /// Take one unit of the current block, switching first if it is used up.
    /// Returns the bits spent switching (0 when no switch happened).
    fn take(&mut self, br: &mut Bits<'_>) -> Result<u64, InspectError> {
        let mut bits = 0;
        if self.left == 0 {
            let at = br.pos;
            let Some((types, counts)) = &self.codes else {
                return Err(br.err("block count exhausted"));
            };
            let next = match u32::from(types.decode(br)?) {
                0 => self.prev,
                1 => (self.cur + 1) % self.n,
                c => c - 2,
            };
            self.prev = self.cur;
            self.cur = next;
            self.left = read_block_count(br, counts)?;
            bits = br.pos - at;
        }
        self.left -= 1;
        Ok(bits)
    }
}

/// Read a context map of `size` entries over `ntrees` (>= 2) trees (§7.3).
fn read_context_map(br: &mut Bits<'_>, size: usize, ntrees: u32) -> Result<Vec<u8>, InspectError> {
    let rlemax = if br.read(1)? == 1 { br.read(4)? + 1 } else { 0 };
    let code = read_prefix(br, (ntrees + rlemax) as usize)?;
    let mut map = vec![0u8; size];
    let mut i = 0;
    while i < size {
        let s = u32::from(code.decode(br)?);
        if s == 0 {
            i += 1;
        } else if s <= rlemax {
            let run = ((1u32 << s) + br.read(s)?) as usize;
            if run > size - i {
                return Err(br.err("context map run overflows the map"));
            }
            i += run;
        } else {
            // s - rlemax < ntrees <= 256.
            map[i] = (s - rlemax) as u8;
            i += 1;
        }
    }
    if br.read(1)? == 1 {
        // Inverse move-to-front.
        let mut mtf: [u8; 256] = core::array::from_fn(|i| i as u8);
        for v in &mut map {
            let idx = usize::from(*v);
            let value = mtf[idx];
            *v = value;
            mtf.copy_within(0..idx, 1);
            mtf[0] = value;
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Literal context lookup (§7.1).

const fn lut0() -> [u8; 256] {
    const HEAD: [u8; 128] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 4, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        8, 12, 16, 12, 12, 20, 12, 16, 24, 28, 12, 12, 32, 12, 36, 12, //
        44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 32, 32, 24, 40, 28, 12, //
        12, 48, 52, 52, 52, 48, 52, 52, 52, 48, 52, 52, 52, 52, 52, 48, //
        52, 52, 52, 52, 52, 48, 52, 52, 52, 52, 52, 24, 12, 28, 12, 12, //
        12, 56, 60, 60, 60, 56, 60, 60, 60, 56, 60, 60, 60, 60, 60, 56, //
        60, 60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 24, 12, 28, 12, 0, //
    ];
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = if i < 128 {
            HEAD[i]
        } else if i < 192 {
            (i & 1) as u8
        } else {
            2 + (i & 1) as u8
        };
        i += 1;
    }
    t
}

const fn lut1() -> [u8; 256] {
    const HEAD: [u8; 128] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, //
        1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, //
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, //
        1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, //
        3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 0, //
    ];
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // 0xe0..=0xff (three- and four-byte UTF-8 leads) are class 2; the
        // other high bytes are class 0.
        t[i] = if i < 128 {
            HEAD[i]
        } else if i < 224 {
            0
        } else {
            2
        };
        i += 1;
    }
    t
}

const fn lut2() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = match i {
            0 => 0,
            1..=15 => 1,
            16..=63 => 2,
            64..=127 => 3,
            128..=191 => 4,
            192..=239 => 5,
            240..=254 => 6,
            _ => 7,
        };
        i += 1;
    }
    t
}

const LUT0: [u8; 256] = lut0();
const LUT1: [u8; 256] = lut1();
const LUT2: [u8; 256] = lut2();

fn literal_context(mode: u8, p1: u8, p2: u8) -> usize {
    let (p1, p2) = (usize::from(p1), usize::from(p2));
    match mode {
        0 => p1 & 0x3f,
        1 => p1 >> 2,
        2 => usize::from(LUT0[p1] | LUT1[p2]),
        _ => usize::from(LUT2[p1] << 3 | LUT2[p2]),
    }
}

// ---------------------------------------------------------------------------
// Insert-and-copy lengths (§5).

/// (insert code base, copy code base) per 64-symbol cell of the command code.
#[rustfmt::skip]
const CELLS: [(usize, usize); 11] = [
    (0, 0), (0, 8), (0, 0), (0, 8), (8, 0), (8, 8),
    (0, 16), (16, 0), (8, 16), (16, 8), (16, 16),
];

/// Insert length codes: (offset, extra bits).
#[rustfmt::skip]
const INSERT: [(u32, u32); 24] = [
    (0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0),
    (6, 1), (8, 1), (10, 2), (14, 2), (18, 3), (26, 3),
    (34, 4), (50, 4), (66, 5), (98, 5), (130, 6), (194, 7),
    (322, 8), (578, 9), (1090, 10), (2114, 12), (6210, 14), (22594, 24),
];

/// Copy length codes: (offset, extra bits).
#[rustfmt::skip]
const COPY: [(u32, u32); 24] = [
    (2, 0), (3, 0), (4, 0), (5, 0), (6, 0), (7, 0),
    (8, 0), (9, 0), (10, 1), (12, 1), (14, 2), (18, 2),
    (22, 3), (30, 3), (38, 4), (54, 4), (70, 5), (102, 5),
    (134, 6), (198, 7), (326, 8), (582, 9), (1094, 10), (2118, 24),
];

// ---------------------------------------------------------------------------
// Static dictionary transforms (§8, Appendix B).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Xf {
    Identity,
    OmitLast(u8),
    UppercaseFirst,
    UppercaseAll,
    OmitFirst(u8),
}

use Xf::{
    Identity as Id, OmitFirst as OF, OmitLast as OL, UppercaseAll as UA, UppercaseFirst as UF,
};

/// RFC 7932 Appendix B: (prefix, transform, suffix) by transform ID.
const TRANSFORMS: [(&[u8], Xf, &[u8]); 121] = [
    (b"", Id, b""),
    (b"", Id, b" "),
    (b" ", Id, b" "),
    (b"", OF(1), b""),
    (b"", UF, b" "),
    (b"", Id, b" the "),
    (b" ", Id, b""),
    (b"s ", Id, b" "),
    (b"", Id, b" of "),
    (b"", UF, b""),
    (b"", Id, b" and "),
    (b"", OF(2), b""),
    (b"", OL(1), b""),
    (b", ", Id, b" "),
    (b"", Id, b", "),
    (b" ", UF, b" "),
    (b"", Id, b" in "),
    (b"", Id, b" to "),
    (b"e ", Id, b" "),
    (b"", Id, b"\""),
    (b"", Id, b"."),
    (b"", Id, b"\">"),
    (b"", Id, b"\n"),
    (b"", OL(3), b""),
    (b"", Id, b"]"),
    (b"", Id, b" for "),
    (b"", OF(3), b""),
    (b"", OL(2), b""),
    (b"", Id, b" a "),
    (b"", Id, b" that "),
    (b" ", UF, b""),
    (b"", Id, b". "),
    (b".", Id, b""),
    (b" ", Id, b", "),
    (b"", OF(4), b""),
    (b"", Id, b" with "),
    (b"", Id, b"'"),
    (b"", Id, b" from "),
    (b"", Id, b" by "),
    (b"", OF(5), b""),
    (b"", OF(6), b""),
    (b" the ", Id, b""),
    (b"", OL(4), b""),
    (b"", Id, b". The "),
    (b"", UA, b""),
    (b"", Id, b" on "),
    (b"", Id, b" as "),
    (b"", Id, b" is "),
    (b"", OL(7), b""),
    (b"", OL(1), b"ing "),
    (b"", Id, b"\n\t"),
    (b"", Id, b":"),
    (b" ", Id, b". "),
    (b"", Id, b"ed "),
    (b"", OF(9), b""),
    (b"", OF(7), b""),
    (b"", OL(6), b""),
    (b"", Id, b"("),
    (b"", UF, b", "),
    (b"", OL(8), b""),
    (b"", Id, b" at "),
    (b"", Id, b"ly "),
    (b" the ", Id, b" of "),
    (b"", OL(5), b""),
    (b"", OL(9), b""),
    (b" ", UF, b", "),
    (b"", UF, b"\""),
    (b".", Id, b"("),
    (b"", UA, b" "),
    (b"", UF, b"\">"),
    (b"", Id, b"=\""),
    (b" ", Id, b"."),
    (b".com/", Id, b""),
    (b" the ", Id, b" of the "),
    (b"", UF, b"'"),
    (b"", Id, b". This "),
    (b"", Id, b","),
    (b".", Id, b" "),
    (b"", UF, b"("),
    (b"", UF, b"."),
    (b"", Id, b" not "),
    (b" ", Id, b"=\""),
    (b"", Id, b"er "),
    (b" ", UA, b" "),
    (b"", Id, b"al "),
    (b" ", UA, b""),
    (b"", Id, b"='"),
    (b"", UA, b"\""),
    (b"", UF, b". "),
    (b" ", Id, b"("),
    (b"", Id, b"ful "),
    (b" ", UF, b". "),
    (b"", Id, b"ive "),
    (b"", Id, b"less "),
    (b"", UA, b"'"),
    (b"", Id, b"est "),
    (b" ", UF, b"."),
    (b"", UA, b"\">"),
    (b" ", Id, b"='"),
    (b"", UF, b","),
    (b"", Id, b"ize "),
    (b"", UA, b"."),
    (b"\xc2\xa0", Id, b""),
    (b" ", Id, b","),
    (b"", UF, b"=\""),
    (b"", UA, b"=\""),
    (b"", Id, b"ous "),
    (b"", UA, b", "),
    (b"", UF, b"='"),
    (b" ", UF, b","),
    (b" ", UA, b"=\""),
    (b" ", UA, b", "),
    (b"", UA, b","),
    (b"", UA, b"("),
    (b"", UA, b". "),
    (b" ", UA, b"."),
    (b"", UA, b"='"),
    (b" ", UA, b". "),
    (b" ", UF, b"=\""),
    (b" ", UA, b"='"),
    (b" ", UF, b"='"),
];

/// Uppercase one UTF-8-ish character at the start of `p` (§8); returns the
/// number of bytes it spans. Bytes past the end of `p` are not touched.
fn to_upper(p: &mut [u8]) -> usize {
    if p[0] < 0xc0 {
        if p[0].is_ascii_lowercase() {
            p[0] ^= 32;
        }
        1
    } else if p[0] < 0xe0 {
        if let Some(b) = p.get_mut(1) {
            *b ^= 32;
        }
        2
    } else {
        if let Some(b) = p.get_mut(2) {
            *b ^= 5;
        }
        3
    }
}

/// Append transform `id` of `word` to `dst`.
fn transform_word(word: &[u8], id: usize, dst: &mut Vec<u8>) {
    let (prefix, xf, suffix) = TRANSFORMS[id];
    dst.extend_from_slice(prefix);
    let w = match xf {
        Xf::OmitFirst(n) => &word[usize::from(n).min(word.len())..],
        Xf::OmitLast(n) => &word[..word.len().saturating_sub(usize::from(n))],
        _ => word,
    };
    let at = dst.len();
    dst.extend_from_slice(w);
    match xf {
        Xf::UppercaseFirst if !w.is_empty() => {
            to_upper(&mut dst[at..]);
        }
        Xf::UppercaseAll => {
            let mut i = at;
            while i < dst.len() {
                i += to_upper(&mut dst[i..]);
            }
        }
        _ => {}
    }
    dst.extend_from_slice(suffix);
}

// ---------------------------------------------------------------------------
// The decoder.

struct Decoder<'a> {
    br: Bits<'a>,
    out: Vec<u8>,
    max_out: usize,
    /// Window size minus 16 (§9.1).
    max_backward: u64,
    /// Last distances, most recent first.
    dist: [u64; 4],
}

impl Decoder<'_> {
    /// WBITS (§9.1).
    fn read_wbits(&mut self) -> Result<u32, InspectError> {
        let br = &mut self.br;
        if br.read(1)? == 0 {
            return Ok(16);
        }
        let n = br.read(3)?;
        if n != 0 {
            return Ok(17 + n);
        }
        let m = br.read(3)?;
        match m {
            0 => Ok(17),
            1 => Err(InspectError {
                bit: 0,
                reason: "large-window stream (unsupported)",
            }),
            m => Ok(8 + m),
        }
    }

    /// Fail unless `n` more output bytes fit under the cap.
    fn room(&self, n: usize) -> Result<(), InspectError> {
        if n > self.max_out - self.out.len() {
            return Err(self.br.err("decoded output exceeds the limit"));
        }
        Ok(())
    }

    fn meta_block(&mut self) -> Result<MetaBlock, InspectError> {
        let start = self.br.pos;
        let mut mb = MetaBlock::new(start, self.out.len() as u64);
        mb.is_last = self.br.read(1)? == 1;
        if mb.is_last && self.br.read(1)? == 1 {
            mb.kind = BlockKind::EmptyLast;
            mb.header_bits = self.br.pos - start;
            mb.pad_bits = self.br.align()?;
            mb.end_bit = self.br.pos;
            return Ok(mb);
        }
        let nibbles = self.br.read(2)?;
        if nibbles == 3 {
            self.metadata(&mut mb)?;
        } else {
            let nibbles = nibbles + 4;
            let mut mlen = 0u64;
            for i in 0..nibbles {
                let at = self.br.pos;
                let v = self.br.read(4)?;
                if i + 1 == nibbles && nibbles > 4 && v == 0 {
                    return Err(InspectError {
                        bit: at,
                        reason: "MLEN has a zero last nibble",
                    });
                }
                mlen |= u64::from(v) << (4 * i);
            }
            mb.mlen = mlen + 1;
            if !mb.is_last && self.br.read(1)? == 1 {
                self.uncompressed(&mut mb)?;
            } else {
                self.compressed(&mut mb)?;
            }
        }
        if mb.is_last {
            mb.pad_bits += self.br.align()?;
        }
        mb.end_bit = self.br.pos;
        Ok(mb)
    }

    fn metadata(&mut self, mb: &mut MetaBlock) -> Result<(), InspectError> {
        let br = &mut self.br;
        mb.kind = BlockKind::Metadata;
        if br.read(1)? != 0 {
            return Err(br.err("reserved bit set in metadata header"));
        }
        let nbytes = br.read(2)?;
        let mut len = 0u64;
        for i in 0..nbytes {
            let at = br.pos;
            let b = br.read(8)?;
            if i + 1 == nbytes && nbytes > 1 && b == 0 {
                return Err(InspectError {
                    bit: at,
                    reason: "MSKIPLEN has a zero last byte",
                });
            }
            len |= u64::from(b) << (8 * i);
        }
        if nbytes > 0 {
            len += 1;
        }
        mb.header_bits = br.pos - mb.start_bit;
        mb.pad_bits = br.align()?;
        if (br.end - br.pos) / 8 < len {
            return Err(br.err("truncated metadata"));
        }
        br.pos += len * 8;
        mb.metadata_len = len;
        Ok(())
    }

    fn uncompressed(&mut self, mb: &mut MetaBlock) -> Result<(), InspectError> {
        mb.kind = BlockKind::Uncompressed;
        mb.header_bits = self.br.pos - mb.start_bit;
        mb.pad_bits = self.br.align()?;
        let br = &self.br;
        if (br.end - br.pos) / 8 < mb.mlen {
            return Err(br.err("truncated uncompressed meta-block"));
        }
        let n = mb.mlen as usize;
        self.room(n)?;
        let at = (self.br.pos / 8) as usize;
        self.out.extend_from_slice(&self.br.data[at..at + n]);
        self.br.pos += mb.mlen * 8;
        mb.literals = mb.mlen;
        mb.literal_bits = mb.mlen * 8;
        Ok(())
    }

    fn compressed(&mut self, mb: &mut MetaBlock) -> Result<(), InspectError> {
        fn category(br: &mut Bits<'_>, nbltypes: &mut u32) -> Result<BlockSwitch, InspectError> {
            *nbltypes = read_varlen8(br)? + 1;
            BlockSwitch::read(br, *nbltypes)
        }
        let br = &mut self.br;
        let mut bl = category(br, &mut mb.nbltypes[0])?;
        let mut bi = category(br, &mut mb.nbltypes[1])?;
        let mut bd = category(br, &mut mb.nbltypes[2])?;
        mb.npostfix = br.read(2)?;
        mb.ndirect = br.read(4)? << mb.npostfix;
        for _ in 0..mb.nbltypes[0] {
            mb.context_modes.push(br.read(2)? as u8);
        }
        mb.ntrees_lit = read_varlen8(br)? + 1;
        let cmap_l = if mb.ntrees_lit >= 2 {
            read_context_map(br, 64 * mb.nbltypes[0] as usize, mb.ntrees_lit)?
        } else {
            vec![0; 64 * mb.nbltypes[0] as usize]
        };
        mb.ntrees_dist = read_varlen8(br)? + 1;
        let cmap_d = if mb.ntrees_dist >= 2 {
            read_context_map(br, 4 * mb.nbltypes[2] as usize, mb.ntrees_dist)?
        } else {
            vec![0; 4 * mb.nbltypes[2] as usize]
        };
        let lit = (0..mb.ntrees_lit)
            .map(|_| read_prefix(br, 256))
            .collect::<Result<Vec<_>, _>>()?;
        let cmd = (0..mb.nbltypes[1])
            .map(|_| read_prefix(br, 704))
            .collect::<Result<Vec<_>, _>>()?;
        let dist_alphabet = 16 + mb.ndirect as usize + (48 << mb.npostfix);
        let dist = (0..mb.ntrees_dist)
            .map(|_| read_prefix(br, dist_alphabet))
            .collect::<Result<Vec<_>, _>>()?;
        mb.header_bits = br.pos - mb.start_bit;

        let end = mb.out_start + mb.mlen;
        let mut word = Vec::with_capacity(48);
        while (self.out.len() as u64) < end {
            // Insert-and-copy command.
            let s = bi.take(&mut self.br)?;
            mb.block_switch_bits += s;
            mb.block_switches[1] += u64::from(s != 0);
            let at = self.br.pos;
            let code = usize::from(cmd[bi.cur as usize].decode(&mut self.br)?);
            let (ib, cb) = CELLS[code >> 6];
            let (ioff, ibits) = INSERT[ib + ((code >> 3) & 7)];
            let (coff, cbits) = COPY[cb + (code & 7)];
            let ilen = u64::from(ioff + self.br.read(ibits)?);
            let clen = u64::from(coff + self.br.read(cbits)?);
            mb.command_bits += self.br.pos - at;
            mb.commands += 1;

            // Literals.
            if ilen > end - self.out.len() as u64 {
                return Err(self.br.err("insert length exceeds the meta-block"));
            }
            self.room(ilen as usize)?;
            for _ in 0..ilen {
                let s = bl.take(&mut self.br)?;
                mb.block_switch_bits += s;
                mb.block_switches[0] += u64::from(s != 0);
                let n = self.out.len();
                let p1 = if n > 0 { self.out[n - 1] } else { 0 };
                let p2 = if n > 1 { self.out[n - 2] } else { 0 };
                let mode = mb.context_modes[bl.cur as usize];
                let ctx = 64 * bl.cur as usize + literal_context(mode, p1, p2);
                let tree = usize::from(cmap_l[ctx]);
                let at = self.br.pos;
                let byte = lit
                    .get(tree)
                    .ok_or(self.br.err("context map names a missing tree"))?
                    .decode(&mut self.br)?;
                mb.literal_bits += self.br.pos - at;
                self.out.push(byte as u8);
            }
            mb.literals += ilen;
            if self.out.len() as u64 == end {
                break;
            }

            // Distance.
            let (distance, dcode) = if code < 128 {
                (self.dist[0], 0)
            } else {
                let s = bd.take(&mut self.br)?;
                mb.block_switch_bits += s;
                mb.block_switches[2] += u64::from(s != 0);
                let ctx = 4 * bd.cur as usize + clen.min(5) as usize - 2;
                let tree = usize::from(cmap_d[ctx]);
                let at = self.br.pos;
                let dcode = dist
                    .get(tree)
                    .ok_or(self.br.err("context map names a missing tree"))?
                    .decode(&mut self.br)?;
                let d = self.distance(u32::from(dcode), mb.npostfix, mb.ndirect, at)?;
                mb.distance_bits += self.br.pos - at;
                (d, dcode)
            };
            if dcode < 16 {
                mb.last_distance_codes += 1;
            }

            let max_distance = self.max_backward.min(self.out.len() as u64);
            if distance > max_distance {
                // Static dictionary reference (§8).
                let at = self.br.pos;
                if !(4..=24).contains(&clen) {
                    return Err(self.br.err("dictionary reference with invalid length"));
                }
                let len = clen as usize;
                let word_id = distance - max_distance - 1;
                let nbits = u32::from(kBrotliDictionarySizeBitsByLength[len]);
                let index = (word_id & ((1 << nbits) - 1)) as usize;
                let tid = word_id >> nbits;
                if tid >= TRANSFORMS.len() as u64 {
                    return Err(InspectError {
                        bit: at,
                        reason: "dictionary reference with invalid transform",
                    });
                }
                let off = kBrotliDictionaryOffsetsByLength[len] as usize + index * len;
                word.clear();
                transform_word(&kBrotliDictionary[off..off + len], tid as usize, &mut word);
                if word.len() as u64 > end - self.out.len() as u64 {
                    return Err(self.br.err("dictionary word exceeds the meta-block"));
                }
                self.room(word.len())?;
                self.out.extend_from_slice(&word);
                mb.dict_refs += 1;
                mb.dict_bytes += word.len() as u64;
            } else {
                if clen > end - self.out.len() as u64 {
                    return Err(self.br.err("copy length exceeds the meta-block"));
                }
                let n = clen as usize;
                self.room(n)?;
                if dcode != 0 {
                    self.dist = [distance, self.dist[0], self.dist[1], self.dist[2]];
                }
                let src = self.out.len() - distance as usize;
                if distance as usize >= n {
                    self.out.extend_from_within(src..src + n);
                } else {
                    for i in 0..n {
                        let b = self.out[src + i];
                        self.out.push(b);
                    }
                }
                mb.copies += 1;
                mb.copy_bytes += clen;
            }
        }
        Ok(())
    }

    /// Distance for distance code `dcode` (§4); reads any extra bits.
    fn distance(
        &mut self,
        dcode: u32,
        npostfix: u32,
        ndirect: u32,
        at: u64,
    ) -> Result<u64, InspectError> {
        if dcode < 16 {
            let base = if dcode < 4 {
                return Ok(self.dist[dcode as usize]);
            } else if dcode < 10 {
                self.dist[0]
            } else {
                self.dist[1]
            };
            let k = (dcode - 4) % 6;
            let delta = i64::from(k / 2 + 1);
            let d = if k.is_multiple_of(2) {
                base as i64 - delta
            } else {
                base as i64 + delta
            };
            if d <= 0 {
                return Err(InspectError {
                    bit: at,
                    reason: "distance short code yields a non-positive distance",
                });
            }
            return Ok(d as u64);
        }
        if dcode < 16 + ndirect {
            return Ok(u64::from(dcode - 15));
        }
        let x = dcode - ndirect - 16;
        let ndistbits = 1 + (x >> (npostfix + 1));
        let hcode = x >> npostfix;
        let lcode = x & ((1 << npostfix) - 1);
        let offset = ((2 + u64::from(hcode & 1)) << ndistbits) - 4;
        let dextra = u64::from(self.br.read(ndistbits)?);
        Ok(((offset + dextra) << npostfix) + u64::from(lcode) + u64::from(ndirect) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brotli_c::EncoderParams;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    fn html() -> Vec<u8> {
        let mut v =
            b"<!doctype html><html><head><title>Listing</title></head><body><ul>\n".to_vec();
        for i in 0..160 {
            v.extend_from_slice(
                format!(
                    "<li class=\"item item-{}\" data-id=\"{i}\"><a href=\"/p/{}\">Product {} \
                     costs ${}.{:02}</a></li>\n",
                    i % 7,
                    i * 31 + 7,
                    i * 13 % 97,
                    i * 3 % 50,
                    i * 17 % 100
                )
                .as_bytes(),
            );
        }
        v.extend_from_slice(b"</ul></body></html>\n");
        v
    }

    fn random(n: usize, mut seed: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    fn runs() -> Vec<u8> {
        let mut v = vec![b'a'; 20000];
        v.extend(std::iter::repeat_n(0u8, 5000));
        v.extend_from_slice(b"abcabcabcabcabc");
        v.extend(std::iter::repeat_n(0xffu8, 3000));
        v
    }

    /// English prose: the C encoder references the static dictionary here
    /// from quality 5 up.
    fn prose() -> Vec<u8> {
        b"The history of the region is closely connected with the development of \
          international trade. During the nineteenth century, several government \
          officials proposed that the university should establish a department of \
          economics, although many members of the community remained skeptical. \
          Nevertheless, the committee published its first report in October, and \
          the recommendations were adopted without significant opposition. Today \
          the institution is recognized for its research, its museum collections, \
          and its commitment to public education. Visitors frequently describe the \
          library as beautiful; others prefer the gardens, where seasonal festivals \
          attract thousands of families every summer. According to the official \
          website, admission is free for children and students. Questions about \
          accessibility, parking, or guided tours should be directed to the \
          information office before arrival. WELCOME TO THE MUSEUM. Please Note That \
          Photography Is Permitted Only In Designated Areas."
            .to_vec()
    }

    /// Multi-byte UTF-8 text (2-, 3- and 4-byte sequences), so UTF8 context
    /// modeling sees every high-byte class.
    fn utf8() -> Vec<u8> {
        let mut s = String::new();
        for i in 0..120 {
            s.push_str(&format!(
                "<p data-n=\"{i}\">Привет, мир {i}! Ça coûte {} €. 東京都の天気は{}です。 🎉🍣 \
                 Ελληνικά κείμενο, עברית, العربية.</p>\n",
                i * 7 % 100,
                ["晴れ", "雨", "曇り"][i % 3]
            ));
        }
        s.into_bytes()
    }

    fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("empty", Vec::new()),
            ("html", html()),
            ("utf8", utf8()),
            ("random", random(6000, 0x9e3779b97f4a7c15)),
            ("runs", runs()),
            ("prose", prose()),
        ]
    }

    fn flush_points(len: usize) -> Vec<usize> {
        let mut f = vec![len / 5, len / 2 + 3, len * 4 / 5];
        f.retain(|&x| x > 0 && x < len);
        f.dedup();
        f
    }

    /// Structural invariants every inspection must satisfy.
    fn check_structure(ins: &Inspection, enc: &[u8]) {
        assert_eq!(ins.total_bits, enc.len() as u64 * 8);
        let (last, rest) = ins.blocks.split_last().expect("at least one block");
        assert!(last.is_last && rest.iter().all(|b| !b.is_last));
        let mut bit = ins.stream_header_bits;
        let mut out = 0u64;
        for b in &ins.blocks {
            assert_eq!(b.start_bit, bit, "blocks contiguous in bits");
            assert_eq!(b.out_start, out, "blocks contiguous in output");
            let parts = b.header_bits
                + b.pad_bits
                + b.literal_bits
                + b.command_bits
                + b.distance_bits
                + b.block_switch_bits
                + 8 * b.metadata_len;
            assert_eq!(parts, b.end_bit - b.start_bit, "bit accounting: {b:?}");
            if b.kind == BlockKind::Compressed {
                assert_eq!(b.literals + b.copy_bytes + b.dict_bytes, b.mlen);
                assert!(b.commands >= 1);
            }
            bit = b.end_bit;
            out += b.mlen;
        }
        assert_eq!(out, ins.decoded.len() as u64);
        assert_eq!(last.end_bit, ins.total_bits);
        // The last block's meaningful bits end within the final byte.
        if last.kind != BlockKind::Metadata {
            assert!(last.end_bit - last.pad_bits > ins.total_bits - 8);
        }
    }

    #[test]
    fn matches_input_and_c_decoder() {
        let mut kinds = Vec::new();
        let mut prose_dict_refs = 0;
        for (name, input) in fixtures() {
            for q in 0..=11 {
                for lgwin in [10, 16, 17, 18, 22, 24] {
                    for flushes in [Vec::new(), flush_points(input.len())] {
                        let p = EncoderParams::new(q, lgwin);
                        let enc = brotli_c::compress_with_flushes(&p, None, &input, &flushes)
                            .expect("C encoder");
                        let ins = inspect(&enc).unwrap_or_else(|e| {
                            panic!("{name} q{q} w{lgwin} flushes {flushes:?}: {e}")
                        });
                        assert!(ins.decoded == input, "{name} q{q} w{lgwin}");
                        let c = brotli_c::decode(&enc, None).expect("C decoder");
                        assert!(c == ins.decoded);
                        check_structure(&ins, &enc);
                        if q >= 2 {
                            assert_eq!(ins.wbits, lgwin, "{name} q{q}");
                        }
                        for f in &flushes {
                            assert!(
                                ins.blocks.iter().any(|b| b.out_start + b.mlen == *f as u64),
                                "{name} q{q} w{lgwin}: no block ends at flush {f}"
                            );
                        }
                        for b in &ins.blocks {
                            if !kinds.contains(&b.kind) {
                                kinds.push(b.kind);
                            }
                            if name == "prose" {
                                prose_dict_refs += b.dict_refs;
                            }
                        }
                    }
                }
            }
        }
        assert!(prose_dict_refs > 0, "no static dictionary references seen");
        for k in [
            BlockKind::Compressed,
            BlockKind::Uncompressed,
            BlockKind::Metadata,
            BlockKind::EmptyLast,
        ] {
            assert!(kinds.contains(&k), "never saw a {k:?} block");
        }
    }

    /// Streams from our rust-brotli fork (the reference encoder) decode the
    /// same through the inspector and C.
    #[test]
    fn matches_reference_encoder() {
        use crate::reference::{ReferenceParams, compress_with_flushes};
        for (name, input) in fixtures() {
            for q in 0..=11 {
                for lgwin in [10, 18, 22] {
                    let flushes = flush_points(input.len());
                    let p = ReferenceParams::new(q, lgwin);
                    let enc = compress_with_flushes(&p, &input, &flushes).expect("encoder");
                    let ins = inspect(&enc).unwrap_or_else(|e| panic!("{name} q{q} w{lgwin}: {e}"));
                    assert!(ins.decoded == input, "{name} q{q} w{lgwin}");
                    assert!(brotli_c::decode(&enc, None).expect("C decoder") == input);
                    check_structure(&ins, &enc);
                }
            }
        }
    }

    #[test]
    fn transforms_match_brotli_decompressor() {
        use brotli_decompressor::transform::{TransformDictionaryWord, kNumTransforms};
        assert_eq!(kNumTransforms as usize, TRANSFORMS.len());
        let mut words: Vec<&[u8]> = vec![
            b"a",
            b"hello",
            b"\xc3\xa9t\xc3\xa9",
            b"\xe2\x82\xacuro",
            b"z\xc3",
            b"ab\xe2\x82",
        ];
        for len in 4..=24 {
            let off = kBrotliDictionaryOffsetsByLength[len] as usize;
            words.push(&kBrotliDictionary[off..off + len]);
            let n = 1usize << kBrotliDictionarySizeBitsByLength[len];
            let off = off + (n - 1) * len;
            words.push(&kBrotliDictionary[off..off + len]);
        }
        for id in 0..TRANSFORMS.len() {
            for w in &words {
                let mut dst = [0u8; 64];
                let n = TransformDictionaryWord(&mut dst, w, w.len() as i32, id as i32);
                let mut mine = Vec::new();
                transform_word(w, id, &mut mine);
                assert_eq!(&dst[..n as usize], &mine[..], "transform {id} of {w:?}");
            }
        }
    }

    #[test]
    fn context_tables_match_encoder() {
        use brotli::enc::constants::{kSigned3BitContextLookup, kUTF8ContextLookup};
        assert_eq!(&kUTF8ContextLookup[..256], &LUT0[..]);
        assert_eq!(&kUTF8ContextLookup[256..], &LUT1[..]);
        assert_eq!(&kSigned3BitContextLookup[..], &LUT2[..]);
    }

    /// LSB-first bit writer for hand-built streams.
    struct W {
        out: Vec<u8>,
        bits: u32,
    }

    impl W {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                bits: 0,
            }
        }
        fn put(&mut self, n: u32, v: u32) -> &mut Self {
            for i in 0..n {
                if self.bits.is_multiple_of(8) {
                    self.out.push(0);
                }
                let last = self.out.len() - 1;
                self.out[last] |= (((v >> i) & 1) as u8) << (self.bits % 8);
                self.bits += 1;
            }
            self
        }
        fn align(&mut self) -> &mut Self {
            self.bits = self.bits.div_ceil(8) * 8;
            self
        }
        fn bytes(&mut self, b: &[u8]) -> &mut Self {
            assert_eq!(self.bits % 8, 0);
            self.out.extend_from_slice(b);
            self.bits += 8 * b.len() as u32;
            self
        }
    }

    #[test]
    fn metadata_uncompressed_and_empty_last() {
        let mut w = W::new();
        w.put(1, 0); // WBITS 16
        // Metadata: ISLAST 0, MNIBBLES 3, reserved 0, MSKIPBYTES 1, MSKIPLEN-1 = 2.
        w.put(1, 0).put(2, 3).put(1, 0).put(2, 1).put(8, 2).align();
        w.bytes(b"xyz");
        // Uncompressed: ISLAST 0, MNIBBLES 0 (4), MLEN-1 = 2, ISUNCOMPRESSED 1.
        w.put(1, 0).put(2, 0).put(16, 2).put(1, 1).align();
        w.bytes(b"hi!");
        // ISLAST 1, ISLASTEMPTY 1.
        w.put(1, 1).put(1, 1).align();
        let enc = w.out.clone();
        assert_eq!(brotli_c::decode(&enc, None).unwrap(), b"hi!");
        let ins = inspect(&enc).unwrap();
        assert_eq!(ins.decoded, b"hi!");
        let kinds: Vec<_> = ins.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Metadata,
                BlockKind::Uncompressed,
                BlockKind::EmptyLast
            ]
        );
        assert_eq!(ins.blocks[0].metadata_len, 3);
        check_structure(&ins, &enc);

        // Trailing data after the last meta-block.
        let mut bad = enc.clone();
        bad.push(0);
        assert!(brotli_c::decode(&bad, None).is_err());
        assert_eq!(
            inspect(&bad).unwrap_err().reason,
            "data after the last meta-block"
        );
        // Non-zero padding before the uncompressed bytes (bit 7 of byte 7).
        let mut bad = enc.clone();
        bad[7] |= 0x80;
        assert!(brotli_c::decode(&bad, None).is_err());
        assert_eq!(inspect(&bad).unwrap_err().reason, "non-zero padding bits");
    }

    #[test]
    fn zero_bit_prefix_codes() {
        // One compressed last meta-block whose every prefix code has a single
        // symbol: "a" then copy 9 from distance 1.
        let mut w = W::new();
        w.put(1, 0); // WBITS 16
        w.put(1, 1).put(1, 0).put(2, 0).put(16, 9); // ISLAST, MLEN = 10
        w.put(1, 0).put(1, 0).put(1, 0); // NBLTYPES L, I, D = 1
        w.put(2, 0).put(4, 0); // NPOSTFIX, NDIRECT
        w.put(2, 0); // CMODE
        w.put(1, 0).put(1, 0); // NTREESL, NTREESD = 1
        w.put(2, 1).put(2, 0).put(8, u32::from(b'a')); // literal code
        // Command 143: insert code 1 (1), copy code 7 (9), explicit distance.
        w.put(2, 1).put(2, 0).put(10, 143);
        w.put(2, 1).put(2, 0).put(6, 16); // distance code 16, 1 extra bit
        w.put(1, 0); // distance extra bit: distance 1
        w.align();
        let enc = w.out.clone();
        assert_eq!(brotli_c::decode(&enc, None).unwrap(), b"aaaaaaaaaa");
        let ins = inspect(&enc).unwrap();
        assert_eq!(ins.decoded, b"aaaaaaaaaa");
        let b = &ins.blocks[0];
        assert_eq!((b.commands, b.literals, b.copies), (1, 1, 1));
        assert_eq!((b.literal_bits, b.command_bits, b.distance_bits), (0, 0, 1));
        check_structure(&ins, &enc);
    }

    #[test]
    fn rejects_bad_headers() {
        assert_eq!(inspect(&[]).unwrap_err().reason, "truncated stream");
        // 1 000 100: the large-window marker.
        let mut w = W::new();
        w.put(1, 1).put(3, 0).put(3, 1).put(1, 1).put(1, 1).align();
        assert_eq!(
            inspect(&w.out).unwrap_err().reason,
            "large-window stream (unsupported)"
        );
        // WBITS 10 (1 000 010), then an empty last block.
        let mut w = W::new();
        w.put(1, 1).put(3, 0).put(3, 2).put(1, 1).put(1, 1).align();
        let ins = inspect(&w.out).unwrap();
        assert_eq!((ins.wbits, ins.stream_header_bits), (10, 7));
        assert!(brotli_c::decode(&w.out, None).unwrap().is_empty());
    }

    fn valid_streams() -> &'static [Vec<u8>] {
        static S: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
        S.get_or_init(|| {
            let mut input = prose();
            input.extend_from_slice(&html()[..3000]);
            let flushes = flush_points(input.len());
            [(1, 16), (5, 18), (9, 22), (11, 22)]
                .iter()
                .map(|&(q, w)| {
                    let p = EncoderParams::new(q, w);
                    brotli_c::compress_with_flushes(&p, None, &input, &flushes).unwrap()
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..600)) {
            let _ = inspect_with_limit(&data, 1 << 20);
        }

        #[test]
        fn truncations_error(which in 0usize..4, cut in any::<prop::sample::Index>()) {
            let enc = &valid_streams()[which];
            let cut = cut.index(enc.len());
            prop_assert!(inspect(&enc[..cut]).is_err());
        }

        #[test]
        fn bit_flips_never_panic(
            which in 0usize..4,
            flips in proptest::collection::vec(any::<prop::sample::Index>(), 1..4),
        ) {
            let mut enc = valid_streams()[which].clone();
            for f in &flips {
                let bit = f.index(enc.len() * 8);
                enc[bit / 8] ^= 1 << (bit % 8);
            }
            let ours = inspect_with_limit(&enc, 1 << 22);
            let c = brotli_c::decode(&enc, None);
            if let Ok(ins) = &ours {
                // Whatever we accept, the C decoder accepts identically.
                prop_assert!(c.as_ref().is_ok_and(|c| *c == ins.decoded));
            } else {
                prop_assert!(c.is_err(), "C accepts what we reject: {:?}", ours);
            }
        }
    }
}
