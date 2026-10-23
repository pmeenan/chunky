//! The per-flush streaming encoder (M2, design.md "Per-flush meta-blocks").
//!
//! A [`Stream`] takes input with `feed`, explicit logical flushes with
//! `flush` and the end with `finish`. Each meta-block carries its actual
//! length: blocks close at a logical flush, at the configured byte cap
//! (counted from the previous block end, never from feed boundaries) and at
//! EOF. A flush also byte-aligns the output (an empty metadata block when
//! needed) so every encoded byte so far can be delivered. Feeding the same
//! bytes with the same flush positions in different fragments produces
//! identical output (D-014).
//!
//! Memory falls into three classes, accounted separately (D-029): the
//! [`Engine`] is shared by every stream in a process; a [`Workspace`] holds
//! block scratch and is shared by the streams one thread encodes; a
//! [`Stream`] holds only per-response state (retained history, the search
//! hasher, carry bits and the parse cursor).
//!
//! Commands come from an injected parse (absolute input order, split at
//! block boundaries here) or, for the cold path, from the fork's q2-class
//! search over retained history (D-016). Either way the fork verifies every
//! copy and dictionary reference before emitting it (D-017), so a wrong
//! parse costs size, never correctness.

use brotli::enc::StandardAlloc;
pub use brotli::enc::external::{
    BlockReport, CapturedBlock, ExternalCommand, ExternalEncoder, ExternalError, ExternalOptions,
    Reference,
};

/// Per-thread block scratch, reused by every stream the thread encodes (one
/// block at a time). Create one with [`Workspace::default`] and pass it to
/// each stream call that may write a block.
pub struct Workspace {
    fork: brotli::enc::external::Workspace<StandardAlloc>,
    /// The open block's slice of an injected parse.
    block: Vec<ExternalCommand>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            fork: brotli::enc::external::Workspace::new(StandardAlloc::default()),
            block: Vec::new(),
        }
    }
}

/// Encoder options; all part of the determinism contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    /// Declared window (WBITS), 10..=24.
    pub lgwin: u32,
    /// Retained history in bits, 10..=lgwin (D-017). With `compact_ring`
    /// copies reach `(1 << retained_bits) - (1 << block_bits) - 16` bytes
    /// back from a ring of `1 << retained_bits`; without, `(1 <<
    /// retained_bits) - 16` from a ring twice that size.
    pub retained_bits: u32,
    /// Largest meta-block, `1 << block_bits` bytes.
    pub block_bits: u32,
    /// Table construction: 2 fast, 4..=9 greedy, 10..=11 oracle.
    pub table_quality: u32,
    /// Search for blocks no injected command covers: 0 for literals only,
    /// 2..=9 for a reference hasher of that quality over retained history.
    pub search_quality: u32,
    /// Size the ring to the retained history instead of twice it; needs
    /// `block_bits < retained_bits`.
    pub compact_ring: bool,
}

impl StreamOptions {
    /// The cold path: q2 search and fast tables over the declared window
    /// (compact ring). Smaller deployments lower `retained_bits` (D-029).
    pub fn cold(lgwin: u32) -> Self {
        Self {
            lgwin,
            retained_bits: lgwin,
            block_bits: 16,
            table_quality: 2,
            search_quality: 2,
            compact_ring: true,
        }
    }

    /// External commands only, with the given table quality.
    pub fn external(lgwin: u32, table_quality: u32) -> Self {
        Self {
            lgwin,
            retained_bits: lgwin,
            block_bits: 16,
            table_quality,
            search_quality: 0,
            compact_ring: true,
        }
    }

    fn fork(&self) -> ExternalOptions {
        ExternalOptions {
            lgwin: self.lgwin,
            retained_bits: self.retained_bits,
            block_bits: self.block_bits,
            table_quality: self.table_quality,
            search_quality: self.search_quality,
            compact_ring: self.compact_ring,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamError {
    BadOptions(&'static str),
    Finished,
    Encoder(ExternalError),
}

impl core::fmt::Display for StreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StreamError::BadOptions(s) => write!(f, "bad options: {s}"),
            StreamError::Finished => f.write_str("stream already finished"),
            StreamError::Encoder(e) => write!(f, "encoder: {e:?}"),
        }
    }
}

impl std::error::Error for StreamError {}

impl From<ExternalError> for StreamError {
    fn from(e: ExternalError) -> Self {
        match e {
            ExternalError::BadOptions(s) => StreamError::BadOptions(s),
            ExternalError::Finished => StreamError::Finished,
            e => StreamError::Encoder(e),
        }
    }
}

/// Response-independent state shared by every stream (D-025). In M2 it
/// holds only the options; models, warm caches and dictionaries join it in
/// M3/M4 as immutable artifacts.
#[derive(Clone, Debug)]
pub struct Engine {
    opts: StreamOptions,
}

impl Engine {
    pub fn new(opts: StreamOptions) -> Result<Self, StreamError> {
        // Validate once, so streams cannot fail on options.
        ExternalEncoder::new(StandardAlloc::default(), opts.fork())?;
        Ok(Self { opts })
    }

    pub fn options(&self) -> &StreamOptions {
        &self.opts
    }

    pub fn stream(&self) -> Stream<'_> {
        Stream::new(self, None)
    }

    /// A stream that takes its commands from `parse`, a command list over
    /// the whole input in order (for example a captured q11 parse).
    pub fn stream_with_parse(&self, parse: Vec<ExternalCommand>) -> Stream<'_> {
        Stream::new(self, Some(InjectedParse::new(parse)))
    }
}

/// Totals over a stream's meta-blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub blocks: u32,
    /// Byte-alignment padding blocks written for flushes.
    pub paddings: u32,
    pub commands: u64,
    pub literals: u64,
    pub window_copies: u64,
    pub dictionary_copies: u64,
    pub rejected_copies: u64,
    pub rejected_bytes: u64,
    pub uncovered_bytes: u64,
    pub overrun_bytes: u64,
    pub hints_used: u64,
    pub hints_ignored: u64,
    pub max_distance: u32,
}

impl StreamStats {
    fn add(&mut self, r: &BlockReport) {
        if r.len > 0 {
            self.blocks += 1;
        }
        self.commands += u64::from(r.commands);
        self.literals += u64::from(r.literals);
        self.window_copies += u64::from(r.window_copies);
        self.dictionary_copies += u64::from(r.dictionary_copies);
        self.rejected_copies += u64::from(r.rejected_copies);
        self.rejected_bytes += u64::from(r.rejected_bytes);
        self.uncovered_bytes += u64::from(r.uncovered_bytes);
        self.overrun_bytes += r.overrun_bytes;
        self.hints_used += u64::from(r.hints_used);
        self.hints_ignored += u64::from(r.hints_ignored);
        self.max_distance = self.max_distance.max(r.max_distance);
    }
}

/// One response's encoder state. Reusable: `reset` returns it to a fresh
/// stream without rebuilding the engine.
pub struct Stream<'e> {
    engine: &'e Engine,
    enc: ExternalEncoder<StandardAlloc>,
    parse: Option<InjectedParse>,
    stats: StreamStats,
    finished: bool,
}

impl<'e> Stream<'e> {
    fn new(engine: &'e Engine, parse: Option<InjectedParse>) -> Self {
        Self {
            engine,
            enc: new_encoder(&engine.opts),
            parse,
            stats: StreamStats::default(),
            finished: false,
        }
    }

    /// Start a new response on this stream's engine, keeping the ring and
    /// hasher allocations (D-025 slot reuse). Output is identical to a fresh
    /// stream's.
    pub fn reset(&mut self, parse: Option<Vec<ExternalCommand>>) {
        self.enc.reset();
        self.parse = parse.map(InjectedParse::new);
        self.stats = StreamStats::default();
        self.finished = false;
    }

    /// The shared engine this stream reads (its artifacts, from M3 on).
    pub fn engine(&self) -> &'e Engine {
        self.engine
    }

    pub fn stats(&self) -> &StreamStats {
        &self.stats
    }

    /// Bytes fed so far.
    pub fn position(&self) -> u64 {
        self.enc.input_pos()
    }

    /// Add input, appending any meta-blocks the byte cap closes to `out`.
    pub fn feed(
        &mut self,
        ws: &mut Workspace,
        mut input: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::Finished);
        }
        while !input.is_empty() {
            let n = self.enc.block_room().min(input.len());
            self.enc.append(&input[..n])?;
            input = &input[n..];
            if self.enc.block_room() == 0 {
                self.close_block(ws, false, out)?;
            }
        }
        Ok(())
    }

    /// Logical flush: close the open block and byte-align, so `out` holds
    /// every byte needed to decode all input so far. Empty and repeated
    /// flushes write nothing.
    pub fn flush(&mut self, ws: &mut Workspace, out: &mut Vec<u8>) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::Finished);
        }
        self.close_block(ws, false, out)?;
        if self.enc.flush(out)? > 0 {
            self.stats.paddings += 1;
        }
        Ok(())
    }

    /// Close the open block without aligning (no delivery promise). Used
    /// to replay a captured block layout.
    pub fn cut(&mut self, ws: &mut Workspace, out: &mut Vec<u8>) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::Finished);
        }
        self.close_block(ws, false, out)
    }

    /// End the stream: the open block becomes the last meta-block (or an
    /// empty last meta-block is written).
    pub fn finish(&mut self, ws: &mut Workspace, out: &mut Vec<u8>) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::Finished);
        }
        self.close_block(ws, true, out)?;
        self.finished = true;
        if let Some(p) = &self.parse {
            self.stats.overrun_bytes += p.remaining();
        }
        Ok(())
    }

    fn close_block(
        &mut self,
        ws: &mut Workspace,
        is_last: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), StreamError> {
        let len = self.enc.pending();
        let report = match self.parse.as_mut() {
            Some(p) if len > 0 => {
                ws.block.clear();
                p.take(len as u64, &mut ws.block);
                self.enc
                    .encode_block(&mut ws.fork, Some(&ws.block), is_last, out)?
            }
            _ => self.enc.encode_block(&mut ws.fork, None, is_last, out)?,
        };
        self.stats.add(&report);
        Ok(())
    }
}

fn new_encoder(opts: &StreamOptions) -> ExternalEncoder<StandardAlloc> {
    // Engine::new validated these options.
    ExternalEncoder::new(StandardAlloc::default(), opts.fork()).expect("validated options")
}

/// A command list over the whole input, consumed one block at a time.
struct InjectedParse {
    cmds: Vec<ExternalCommand>,
    idx: usize,
    /// Bytes of `cmds[idx]` already consumed.
    used: u64,
}

impl InjectedParse {
    fn new(cmds: Vec<ExternalCommand>) -> Self {
        Self {
            cmds,
            idx: 0,
            used: 0,
        }
    }

    /// Append commands covering the next `len` bytes to `out`, splitting
    /// commands at the block boundary. A split window copy keeps its
    /// distance (the second piece resolves to last-distance code 0); a
    /// piece shorter than a legal copy, and any split dictionary word,
    /// become literals. Short-code hints stay on the first piece only.
    fn take(&mut self, len: u64, out: &mut Vec<ExternalCommand>) {
        let mut room = len;
        let mut insert = 0u64;
        while room > 0 && self.idx < self.cmds.len() {
            let c = self.cmds[self.idx];
            let ins_left = u64::from(c.insert_len).saturating_sub(self.used);
            let copy_done = self.used.saturating_sub(u64::from(c.insert_len));
            let copy_left = u64::from(c.copy_len) - copy_done;
            let ins = ins_left.min(room);
            insert += ins;
            room -= ins;
            self.used += ins;
            if ins < ins_left {
                break;
            }
            let take = copy_left.min(room);
            let whole = copy_done == 0 && take == copy_left;
            let reference = match c.reference {
                Reference::Window {
                    distance,
                    short_code,
                } if take >= 2 && !whole => Some(Reference::Window {
                    distance,
                    short_code: if copy_done == 0 { short_code } else { None },
                }),
                r if whole && take > 0 => Some(r),
                _ => None,
            };
            match reference {
                Some(reference) => {
                    out.push(ExternalCommand {
                        insert_len: insert as u32,
                        copy_len: take as u32,
                        reference,
                    });
                    insert = 0;
                }
                None => insert += take,
            }
            room -= take;
            self.used += take;
            if self.used == c.len() {
                self.idx += 1;
                self.used = 0;
            }
        }
        // Bytes past the end of the parse stay uncovered: the encoder emits
        // them as literals and reports them.
        if insert > 0 {
            out.push(ExternalCommand::literals(insert as u32));
        }
    }

    /// Parse bytes not consumed yet.
    fn remaining(&self) -> u64 {
        self.cmds[self.idx.min(self.cmds.len())..]
            .iter()
            .map(ExternalCommand::len)
            .sum::<u64>()
            - self.used
    }
}

/// Encode `input` with logical flushes after each offset in `flushes`
/// (ascending; offsets past the end, empty and repeated ones are no-ops),
/// then finish.
pub fn encode_with_flushes(
    engine: &Engine,
    ws: &mut Workspace,
    parse: Option<Vec<ExternalCommand>>,
    input: &[u8],
    flushes: &[usize],
) -> Result<(Vec<u8>, StreamStats), StreamError> {
    let mut s = match parse {
        Some(p) => engine.stream_with_parse(p),
        None => engine.stream(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(input.len());
        if f >= pos {
            s.feed(ws, &input[pos..f], &mut out)?;
            s.flush(ws, &mut out)?;
            pos = f;
        }
    }
    s.feed(ws, &input[pos..], &mut out)?;
    s.finish(ws, &mut out)?;
    Ok((out, s.stats))
}

/// Flatten captured meta-blocks into one command list over the input.
pub fn flatten_parse(blocks: &[CapturedBlock]) -> Vec<ExternalCommand> {
    blocks
        .iter()
        .flat_map(|b| b.commands.iter().copied())
        .collect()
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
