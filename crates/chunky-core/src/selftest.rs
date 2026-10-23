//! Native-vs-wasm byte identity (M2 exit): one digest over the encoded
//! output of every stream path on the synthetic fixtures. The regular
//! checks compute it natively and in V8 and require the two to match.

use crate::fixtures::fixtures;
use crate::reference::{ReferenceParams, capture_parse};
use crate::stream::{
    Engine, ExternalCommand, StreamOptions, Workspace, encode_with_flushes, flatten_parse,
};

/// FNV-1a, 64-bit.
struct Fnv(u64);

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn write_len(&mut self, n: usize) {
        self.write(&(n as u64).to_le_bytes());
    }
}

/// Encode with the same logical flushes as `encode_with_flushes`, feeding
/// in pieces of 1, 7, 61, 509 and 4093 bytes in turn.
fn encode_fragmented(
    e: &Engine,
    parse: Option<Vec<ExternalCommand>>,
    input: &[u8],
    flushes: &[usize],
) -> Vec<u8> {
    const PIECES: [usize; 5] = [1, 7, 61, 509, 4093];
    let mut ws = Workspace::default();
    let mut s = match parse {
        Some(p) => e.stream_with_parse(p),
        None => e.stream(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    let mut k = 0;
    let ends = flushes
        .iter()
        .map(|&f| (f, true))
        .chain([(input.len(), false)]);
    for (end, flush) in ends {
        while pos < end {
            let n = PIECES[k % PIECES.len()].min(end - pos);
            k += 1;
            s.feed(&mut ws, &input[pos..pos + n], &mut out)
                .expect("feed");
            pos += n;
        }
        if flush {
            s.flush(&mut ws, &mut out).expect("flush");
        }
    }
    s.finish(&mut ws, &mut out).expect("finish");
    out
}

/// Digest of every configuration's output on every fixture (each cut to
/// 16 KiB to keep debug builds quick), plus whether fragmented feeding
/// reproduced each output (it must; a mismatch panics, which traps in
/// wasm).
pub fn identity_digest() -> u64 {
    let configs = [
        StreamOptions::cold(22),
        StreamOptions {
            retained_bits: 12,
            block_bits: 10,
            ..StreamOptions::cold(16)
        },
        StreamOptions {
            table_quality: 5,
            search_quality: 5,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            table_quality: 11,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            compact_ring: false,
            ..StreamOptions::cold(18)
        },
    ];
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    for (name, mut input) in fixtures() {
        input.truncate(16 << 10);
        let flushes: Vec<usize> = (0..input.len()).step_by(3001).collect();
        h.write(name.as_bytes());
        for opts in configs {
            let e = Engine::new(opts).expect("valid options");
            let (out, _) =
                encode_with_flushes(&e, &mut Workspace::default(), None, &input, &flushes)
                    .expect("encode");
            h.write_len(out.len());
            h.write(&out);
            let same = encode_fragmented(&e, None, &input, &flushes) == out;
            assert!(same, "fragmented feeding changed the output");
        }
        // The reference q11 stream, its captured parse re-injected with
        // oracle and fast tables.
        let (reference, blocks) =
            capture_parse(&ReferenceParams::new(11, 22), &input, &flushes).expect("q11");
        h.write_len(reference.len());
        h.write(&reference);
        for table_quality in [2, 11] {
            let e = Engine::new(StreamOptions::external(22, table_quality)).expect("options");
            let parse = flatten_parse(&blocks);
            let (out, _) = encode_with_flushes(
                &e,
                &mut Workspace::default(),
                Some(parse.clone()),
                &input,
                &flushes,
            )
            .expect("encode");
            h.write_len(out.len());
            h.write(&out);
            let same = encode_fragmented(&e, Some(parse), &input, &flushes) == out;
            assert!(same, "fragmented feeding changed the output");
        }
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::mixed;

    #[test]
    fn digest_is_stable_within_a_build() {
        assert_eq!(identity_digest(), identity_digest());
    }

    #[test]
    fn fragmented_feeding_matches() {
        let input = mixed(20_000);
        let e = Engine::new(StreamOptions::cold(22)).unwrap();
        let flushes = [0, 100, 5000, 5000, 19_999];
        let (out, _) =
            encode_with_flushes(&e, &mut Workspace::default(), None, &input, &flushes).unwrap();
        assert!(encode_fragmented(&e, None, &input, &flushes) == out);
    }
}
