//! The rust-brotli reference encoder at the fork base, driven with the same
//! call/flush sequence as the C baseline (D-016). It is the M1 reference
//! curve's Rust side and the q2-class cold path's starting point.

use brotli::enc::StandardAlloc;
use brotli::enc::encode::{
    BrotliEncoderOperation, BrotliEncoderParameter, BrotliEncoderStateStruct,
};
use brotli::enc::interface::{PredictionModeContextMap, StaticCommand};
use brotli::{InputPair, InputReferenceMut};

/// Encoder parameters, mapped one-to-one onto `BrotliEncoderParameter`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceParams {
    pub quality: u32,
    pub lgwin: u32,
    /// 0 generic, 1 text, 2 font.
    pub mode: u32,
    pub lgblock: Option<u32>,
    pub disable_literal_context_modeling: bool,
    pub size_hint: Option<u32>,
}

impl ReferenceParams {
    pub fn new(quality: u32, lgwin: u32) -> Self {
        Self {
            quality,
            lgwin,
            mode: 0,
            lgblock: None,
            disable_literal_context_modeling: false,
            size_hint: None,
        }
    }
}

/// Stream operation, as in `BrotliEncoderOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Process,
    Flush,
    Finish,
}

#[derive(Debug, PartialEq, Eq)]
pub struct EncodeError(pub &'static str);

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for EncodeError {}

const OUT_CHUNK: usize = 1 << 16;

/// One rust-brotli encoder stream.
pub struct ReferenceEncoder {
    state: BrotliEncoderStateStruct<StandardAlloc>,
    scratch: Vec<u8>,
}

impl ReferenceEncoder {
    pub fn new(p: &ReferenceParams) -> Result<Self, EncodeError> {
        use BrotliEncoderParameter::*;
        let mut state = BrotliEncoderStateStruct::new(StandardAlloc::default());
        let mut ok = state.set_parameter(BROTLI_PARAM_QUALITY, p.quality)
            && state.set_parameter(BROTLI_PARAM_LGWIN, p.lgwin)
            && state.set_parameter(BROTLI_PARAM_MODE, p.mode);
        if let Some(b) = p.lgblock {
            ok &= state.set_parameter(BROTLI_PARAM_LGBLOCK, b);
        }
        if p.disable_literal_context_modeling {
            ok &= state.set_parameter(BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING, 1);
        }
        if let Some(h) = p.size_hint {
            ok &= state.set_parameter(BROTLI_PARAM_SIZE_HINT, h);
        }
        if !ok {
            return Err(EncodeError("parameter rejected"));
        }
        Ok(Self {
            state,
            scratch: vec![0; OUT_CHUNK],
        })
    }

    /// Record each meta-block's parse from here on (q2 and up; see
    /// `take_parse_log`).
    pub fn enable_parse_log(&mut self) {
        self.state.enable_parse_log();
    }

    /// The meta-blocks written since the last call, as logical commands
    /// with their distance short codes.
    pub fn take_parse_log(&mut self) -> Vec<brotli::enc::external::CapturedBlock> {
        self.state.take_parse_log()
    }

    /// Feed `input` with `op` and append everything the encoder emits to
    /// `out`. Flush and Finish run until the operation completes.
    pub fn push(&mut self, input: &[u8], op: Op, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let op = match op {
            Op::Process => BrotliEncoderOperation::BROTLI_OPERATION_PROCESS,
            Op::Flush => BrotliEncoderOperation::BROTLI_OPERATION_FLUSH,
            Op::Finish => BrotliEncoderOperation::BROTLI_OPERATION_FINISH,
        };
        let mut avail_in = input.len();
        let mut in_off = 0;
        let mut nop = |_: &mut PredictionModeContextMap<InputReferenceMut>,
                       _: &mut [StaticCommand],
                       _: InputPair,
                       _: &mut StandardAlloc| ();
        loop {
            let mut avail_out = self.scratch.len();
            let mut out_off = 0;
            let mut total = None;
            if !self.state.compress_stream(
                op,
                &mut avail_in,
                input,
                &mut in_off,
                &mut avail_out,
                &mut self.scratch,
                &mut out_off,
                &mut total,
                &mut nop,
            ) {
                return Err(EncodeError("compress_stream failed"));
            }
            out.extend_from_slice(&self.scratch[..out_off]);
            if avail_in == 0 && !self.state.has_more_output() {
                if op == BrotliEncoderOperation::BROTLI_OPERATION_FINISH
                    && !self.state.is_finished()
                {
                    continue;
                }
                return Ok(());
            }
        }
    }
}

/// Compress `input`, flushing after each offset in `flushes` (ascending;
/// empty or repeated positions are no-ops), then finish.
pub fn compress_with_flushes(
    p: &ReferenceParams,
    input: &[u8],
    flushes: &[usize],
) -> Result<Vec<u8>, EncodeError> {
    let mut e = ReferenceEncoder::new(p)?;
    let mut out = Vec::new();
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(input.len());
        if f > pos {
            e.push(&input[pos..f], Op::Flush, &mut out)?;
            pos = f;
        }
    }
    e.push(&input[pos..], Op::Finish, &mut out)?;
    Ok(out)
}

/// Like [`compress_with_flushes`], also returning the reference encoder's
/// parse, one entry per meta-block (q2 and up; q0/q1 record nothing).
pub fn capture_parse(
    p: &ReferenceParams,
    input: &[u8],
    flushes: &[usize],
) -> Result<(Vec<u8>, Vec<brotli::enc::external::CapturedBlock>), EncodeError> {
    let mut e = ReferenceEncoder::new(p)?;
    e.enable_parse_log();
    let mut out = Vec::new();
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(input.len());
        if f > pos {
            e.push(&input[pos..f], Op::Flush, &mut out)?;
            pos = f;
        }
    }
    e.push(&input[pos..], Op::Finish, &mut out)?;
    let log = e.take_parse_log();
    Ok((out, log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn templated(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            v.extend_from_slice(
                format!(
                    "<div class=\"r\" data-k=\"{}\"><a href=\"/p/{i}\">item {i}</a></div>\n",
                    i * 31 % 97
                )
                .as_bytes(),
            );
        }
        v
    }

    #[test]
    fn round_trips_through_c_at_every_quality() {
        let input = templated(3000);
        for q in 0..=11 {
            let out =
                compress_with_flushes(&ReferenceParams::new(q, 22), &input, &[15, 10_000, 50_000])
                    .unwrap();
            assert_eq!(brotli_c::decode(&out, None).unwrap(), input, "q{q}");
        }
    }

    /// Templated rows with pseudo-random words and ids, so matches are short
    /// to medium and many come from positions the hasher bulk-stored inside
    /// earlier copies.
    fn noisy_templated(len: usize) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
        ];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) as usize
        };
        let mut v = Vec::with_capacity(len + 256);
        while v.len() < len {
            let (a, b, c, id) = (next() % 16, next() % 16, next() % 16, next() % 100_000);
            v.extend_from_slice(
                format!(
                    "<li class=\"item {}\"><a href=\"/{}/{id}\">{} {}</a><span>{id}</span></li>\n",
                    WORDS[a], WORDS[b], WORDS[c], WORDS[a]
                )
                .as_bytes(),
            );
        }
        v.truncate(len);
        v
    }

    /// RE-009: past the 2^(lgwin+1) ring buffer, rust-brotli's bulk hash
    /// stores wrote masked positions, so q2-q5 lost up to 20% against C at
    /// lgwin 18. Sizes must track pinned C on an input that wraps it twice.
    #[test]
    fn tracks_c_after_ring_buffer_wrap() {
        let input = noisy_templated(5 << 18);
        for q in 2..=5 {
            let rust = compress_with_flushes(&ReferenceParams::new(q, 18), &input, &[]).unwrap();
            let mut c = Vec::new();
            let mut e = brotli_c::Encoder::new(&brotli_c::EncoderParams::new(q, 18)).unwrap();
            e.push(&input, brotli_c::Op::Finish, &mut c).unwrap();
            assert_eq!(brotli_c::decode(&rust, None).unwrap(), input, "q{q}");
            let ratio = rust.len() as f64 / c.len() as f64;
            assert!(
                ratio < 1.01,
                "q{q}: rust {} vs C {} bytes ({ratio:.4})",
                rust.len(),
                c.len()
            );
        }
    }

    /// rust-brotli's q10/q11 UTF-8 test read the ring buffer two bytes early
    /// (fixed in the fork, `crates/brotli/CHUNKY.md`). 1,000 bytes with 249
    /// lone 0xFF bytes and two ASCII bytes last: C counts 751 UTF-8 bytes,
    /// above its 75% cutoff (UTF-8 context mode). The shifted window swapped
    /// the last two bytes for the buffer's two leading NULs, which brotli
    /// does not count as UTF-8, so it saw 749 (signed).
    #[test]
    fn literal_context_mode_matches_c_at_the_utf8_threshold() {
        let mut input: Vec<u8> = (0..998)
            .map(|i| {
                if i % 4 == 3 {
                    0xff
                } else {
                    b'a' + (i % 5) as u8
                }
            })
            .collect();
        input.extend_from_slice(b"zz");
        for q in [10, 11] {
            let rust = compress_with_flushes(&ReferenceParams::new(q, 22), &input, &[]).unwrap();
            let mut c = Vec::new();
            let mut e = brotli_c::Encoder::new(&brotli_c::EncoderParams::new(q, 22)).unwrap();
            e.push(&input, brotli_c::Op::Finish, &mut c).unwrap();
            let modes = |s: &[u8]| -> Vec<Vec<u8>> {
                crate::inspect::inspect(s)
                    .unwrap()
                    .blocks
                    .into_iter()
                    .map(|b| b.context_modes)
                    .filter(|m| !m.is_empty())
                    .collect()
            };
            let (rm, cm) = (modes(&rust), modes(&c));
            assert_eq!(cm, vec![vec![2]], "q{q}: C picks UTF-8");
            assert_eq!(rm, cm, "q{q}: literal context modes");
        }
    }

    #[test]
    fn empty_input() {
        for q in [1, 2, 5, 11] {
            let out = compress_with_flushes(&ReferenceParams::new(q, 22), &[], &[0, 0]).unwrap();
            assert_eq!(brotli_c::decode(&out, None).unwrap(), b"");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn flushed_streams_decode_exactly(
            data in proptest::collection::vec(prop_oneof![Just(b'a'), Just(b'<'), any::<u8>()], 0..4000),
            mut cuts in proptest::collection::vec(0usize..4000, 0..8),
            q in prop_oneof![Just(1u32), Just(2), Just(5), Just(9)],
        ) {
            cuts.sort_unstable();
            let out = compress_with_flushes(&ReferenceParams::new(q, 18), &data, &cuts).unwrap();
            prop_assert_eq!(brotli_c::decode(&out, None).unwrap(), data);
        }
    }
}
