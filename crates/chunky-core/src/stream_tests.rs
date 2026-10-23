use super::*;
use crate::fixtures::*;
use crate::reference::{ReferenceParams, capture_parse, compress_with_flushes};
use proptest::prelude::*;

// ---- helpers ------------------------------------------------------------

fn decode_rust(stream: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    brotli_decompressor::Decompressor::new(stream, 4096)
        .read_to_end(&mut out)
        .expect("rust decoder");
    out
}

/// All three independent decoders (C, brotli-decompressor and the
/// inspector) must return exactly `input`.
fn assert_round_trip(stream: &[u8], input: &[u8], what: &str) {
    let c = brotli_c::decode(stream, None).unwrap_or_else(|e| panic!("{what}: C decode: {e}"));
    assert!(c == input, "{what}: C decode differs");
    assert!(decode_rust(stream) == input, "{what}: Rust decode differs");
    let i = crate::inspect::inspect(stream).unwrap_or_else(|e| panic!("{what}: inspect: {e}"));
    assert!(i.decoded == input, "{what}: inspector decode differs");
}

/// Meta-block structure: data blocks stay within the byte cap and every
/// logical flush offset ends a block.
fn assert_layout(stream: &[u8], block_bits: u32, flushes: &[usize], len: usize, what: &str) {
    use crate::inspect::BlockKind;
    let i = crate::inspect::inspect(stream).unwrap();
    let mut ends = std::collections::HashSet::new();
    for b in &i.blocks {
        if matches!(b.kind, BlockKind::Compressed | BlockKind::Uncompressed) {
            assert!(b.mlen <= 1 << block_bits, "{what}: block of {} B", b.mlen);
            ends.insert(b.out_start + b.mlen);
        }
    }
    // Flushes behind the current position are ignored, as the encoder does.
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(len) as u64;
        if f < pos {
            continue;
        }
        pos = f;
        assert!(
            f == 0 || ends.contains(&f),
            "{what}: no block ends at flush {f}"
        );
    }
}

fn engine(opts: StreamOptions) -> Engine {
    Engine::new(opts).unwrap()
}

/// Encode with flushes, checking after each flush that the output so far
/// plus an empty last meta-block (0x03, byte-aligned) decodes to the input
/// so far: a flush delivers every byte.
fn encode_checking_flushes(
    e: &Engine,
    parse: Option<Vec<ExternalCommand>>,
    input: &[u8],
    flushes: &[usize],
) -> (Vec<u8>, StreamStats) {
    let mut ws = Workspace::default();
    let mut s = match parse {
        Some(p) => e.stream_with_parse(p),
        None => e.stream(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(input.len());
        if f < pos {
            continue;
        }
        s.feed(&mut ws, &input[pos..f], &mut out).unwrap();
        s.flush(&mut ws, &mut out).unwrap();
        pos = f;
        if f == 0 {
            // Nothing encoded yet; even the stream header is still pending.
            continue;
        }
        let mut probe = out.clone();
        probe.push(0x03);
        let got = brotli_c::decode(&probe, None).expect("flushed prefix decodes");
        assert!(got == input[..f], "flush at {f} did not deliver the prefix");
    }
    s.feed(&mut ws, &input[pos..], &mut out).unwrap();
    s.finish(&mut ws, &mut out).unwrap();
    (out, *s.stats())
}

fn schedules(len: usize) -> Vec<Vec<usize>> {
    vec![
        vec![],
        vec![15],
        (1..len).step_by(997).collect(),
        vec![0, 0, 1, 1, 4096, 4096, len / 2, len],
    ]
}

// ---- tests --------------------------------------------------------------

#[test]
fn one_byte_dictionary_copies_round_trip() {
    let mut word = [0; 64];
    let len = brotli::enc::external::expand_dictionary_word(4, 0, 23, &mut word).unwrap();
    assert_eq!(len, 1); // omit the last three bytes of a four-byte word
    let input = vec![word[0]; 1024];
    let parse = vec![
        ExternalCommand {
            insert_len: 0,
            copy_len: 1,
            reference: Reference::Dictionary {
                word_len: 4,
                word_idx: 0,
                transform: 23
            },
        };
        input.len()
    ];
    for table_quality in [2, 5, 11] {
        let (out, stats) = encode_with_flushes(
            &engine(StreamOptions::external(22, table_quality)),
            &mut Workspace::default(),
            Some(parse.clone()),
            &input,
            &[],
        )
        .unwrap();
        assert_round_trip(&out, &input, "one-byte dictionary copies");
        assert_eq!(stats.rejected_bytes, input.len() as u64);
    }
}

#[test]
fn cold_path_round_trips() {
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
            ..StreamOptions::cold(20)
        },
        StreamOptions::external(18, 2), // literals only, no hasher
    ];
    for opts in configs {
        let e = engine(opts);
        for (name, input) in fixtures() {
            for flushes in schedules(input.len()) {
                let (out, stats) = encode_checking_flushes(&e, None, &input, &flushes);
                let what = format!("{name} {opts:?} {flushes:?}");
                assert_round_trip(&out, &input, &what);
                assert_layout(&out, opts.block_bits, &flushes, input.len(), &what);
                assert_eq!(stats.rejected_copies, 0, "{name}: search output verifies");
                assert!(
                    stats.max_distance < (1 << opts.retained_bits) - 15,
                    "{name}: retained cap"
                );
            }
        }
    }
}

#[test]
fn cold_path_tracks_reference_q2() {
    // Not byte-identical (different block layout and retained cap), but the
    // same class: within a few percent of rust-brotli q2.
    let input = templated(200_000);
    let (ours, _) = encode_with_flushes(
        &engine(StreamOptions::cold(22)),
        &mut Workspace::default(),
        None,
        &input,
        &[],
    )
    .unwrap();
    let reference = compress_with_flushes(&ReferenceParams::new(2, 22), &input, &[]).unwrap();
    let ratio = ours.len() as f64 / reference.len() as f64;
    assert!(
        ratio < 1.05,
        "cold {} vs q2 {} ({ratio:.3})",
        ours.len(),
        reference.len()
    );
}

#[test]
fn block_cap_bounds_meta_blocks() {
    let input = templated(10_000);
    let opts = StreamOptions {
        block_bits: 10,
        ..StreamOptions::cold(18)
    };
    let (out, stats) =
        encode_with_flushes(&engine(opts), &mut Workspace::default(), None, &input, &[]).unwrap();
    assert_eq!(stats.blocks, 10_000_u32.div_ceil(1024));
    assert_round_trip(&out, &input, "cap");
    // Cap boundaries count from the previous block end, not from the start.
    let (_, stats) = encode_with_flushes(
        &engine(opts),
        &mut Workspace::default(),
        None,
        &input,
        &[100],
    )
    .unwrap();
    assert_eq!(stats.blocks, 1 + 9_900_u32.div_ceil(1024));
}

#[test]
fn empty_and_repeated_flushes_are_no_ops() {
    let e = engine(StreamOptions::cold(22));
    for input in [&b""[..], b"x", b"<html>"] {
        let (plain, _) =
            encode_with_flushes(&e, &mut Workspace::default(), None, input, &[]).unwrap();
        let (noisy, _) =
            encode_with_flushes(&e, &mut Workspace::default(), None, input, &[0, 0, 0]).unwrap();
        assert_eq!(plain, noisy);
        assert_round_trip(&plain, input, "empty");
        let n = input.len();
        let (a, _) = encode_with_flushes(&e, &mut Workspace::default(), None, input, &[n]).unwrap();
        let (b, _) = encode_with_flushes(
            &e,
            &mut Workspace::default(),
            None,
            input,
            &[n, n, n, n + 5],
        )
        .unwrap();
        assert_eq!(a, b);
        assert_round_trip(&a, input, "repeat");
    }
}

#[test]
fn stream_reset_reproduces_output() {
    let e = engine(StreamOptions::cold(22));
    let input = mixed(30_000);
    let mut ws = Workspace::default();
    let mut s = e.stream();
    let mut first = Vec::new();
    s.feed(&mut ws, &input, &mut first).unwrap();
    s.finish(&mut ws, &mut first).unwrap();
    s.reset(None);
    let mut second = Vec::new();
    s.feed(&mut ws, &input[..7], &mut second).unwrap();
    s.feed(&mut ws, &input[7..], &mut second).unwrap();
    s.finish(&mut ws, &mut second).unwrap();
    assert_eq!(first, second);
    assert!(
        s.feed(&mut ws, b"x", &mut second).is_err(),
        "finished stream rejects input"
    );
}

/// A reset stream reuses its ring and hasher in place and must produce
/// exactly what a fresh stream produces, whatever the previous response
/// left in them: larger or smaller, wrapping the ring or not.
#[test]
fn reset_matches_fresh_streams() {
    let inputs = [
        mixed(70_000),
        b"<p>tiny</p>".to_vec(),
        templated(9_000),
        prose(40_000),
        runs(300),
        mixed(70_000),
    ];
    let configs = [
        StreamOptions::cold(22),
        StreamOptions {
            retained_bits: 14,
            block_bits: 10,
            ..StreamOptions::cold(16)
        },
        StreamOptions {
            search_quality: 5,
            table_quality: 5,
            retained_bits: 15,
            block_bits: 12,
            ..StreamOptions::cold(18)
        },
        StreamOptions {
            search_quality: 9,
            retained_bits: 16,
            block_bits: 12,
            ..StreamOptions::cold(18)
        },
        StreamOptions {
            compact_ring: false,
            retained_bits: 13,
            block_bits: 10,
            ..StreamOptions::cold(16)
        },
    ];
    for opts in configs {
        let e = engine(opts);
        let mut ws = Workspace::default();
        let mut reused = e.stream();
        for (k, input) in inputs.iter().enumerate() {
            let flushes = [input.len() / 3, input.len() / 2];
            let (fresh, _) =
                encode_with_flushes(&e, &mut Workspace::default(), None, input, &flushes).unwrap();
            if k > 0 {
                reused.reset(None);
            }
            let mut out = Vec::new();
            let mut pos = 0;
            for f in flushes {
                reused.feed(&mut ws, &input[pos..f], &mut out).unwrap();
                reused.flush(&mut ws, &mut out).unwrap();
                pos = f;
            }
            reused.feed(&mut ws, &input[pos..], &mut out).unwrap();
            reused.finish(&mut ws, &mut out).unwrap();
            assert!(out == fresh, "{opts:?}: response {k} differs after reset");
        }
    }
}

/// Parse coverage is reported, not hidden: bytes past the end of an
/// injected parse are uncovered, and parse bytes past the input overrun.
#[test]
fn parse_coverage_is_reported() {
    let input = templated(10_000);
    let e = engine(StreamOptions::external(22, 2));
    let (_, short) = encode_with_flushes(
        &e,
        &mut Workspace::default(),
        Some(vec![ExternalCommand::literals(100)]),
        &input,
        &[],
    )
    .unwrap();
    assert_eq!(short.uncovered_bytes, 9_900);
    assert_eq!(short.overrun_bytes, 0);
    let (_, long) = encode_with_flushes(
        &e,
        &mut Workspace::default(),
        Some(vec![ExternalCommand::literals(12_000)]),
        &input,
        &[4000],
    )
    .unwrap();
    assert_eq!(long.uncovered_bytes, 0);
    assert_eq!(long.overrun_bytes, 2_000);
    // An insert ending exactly at a block boundary pushes its copy past the
    // block: overrun, not a rejected copy.
    let opts = StreamOptions {
        block_bits: 10,
        ..StreamOptions::external(22, 2)
    };
    let enc_opts = ExternalOptions {
        lgwin: opts.lgwin,
        retained_bits: opts.retained_bits,
        block_bits: opts.block_bits,
        table_quality: opts.table_quality,
        search_quality: opts.search_quality,
        compact_ring: opts.compact_ring,
    };
    let mut enc = ExternalEncoder::new(brotli::enc::StandardAlloc::default(), enc_opts).unwrap();
    let mut fws = brotli::enc::external::Workspace::new(brotli::enc::StandardAlloc::default());
    enc.append(&input[..1024]).unwrap();
    let mut out = Vec::new();
    let r = enc
        .encode_block(
            &mut fws,
            Some(&[ExternalCommand::window(1024, 50, 1)]),
            true,
            &mut out,
        )
        .unwrap();
    assert_eq!((r.rejected_copies, r.overrun_bytes), (0, 50));
    assert_round_trip(&out, &input[..1024], "boundary");
}

/// A split copy keeps its short-code hint on the first piece.
#[test]
fn split_copy_keeps_first_hint() {
    // 2,000 bytes of a 7-byte period: one long copy at distance 7 crossing
    // two 1 KiB block boundaries, hinted with an explicit distance code.
    let input: Vec<u8> = (0..2000).map(|i| b"abcdefg"[i % 7]).collect();
    let parse = vec![ExternalCommand {
        insert_len: 7,
        copy_len: 1993,
        reference: Reference::Window {
            distance: 7,
            short_code: Some(brotli::enc::external::EXPLICIT_DISTANCE),
        },
    }];
    let opts = StreamOptions {
        block_bits: 10,
        ..StreamOptions::external(16, 2)
    };
    let (out, stats) = encode_with_flushes(
        &engine(opts),
        &mut Workspace::default(),
        Some(parse),
        &input,
        &[],
    )
    .unwrap();
    assert_round_trip(&out, &input, "split hint");
    assert_eq!((stats.hints_used, stats.hints_ignored), (1, 0));
}

/// A q11 parse replayed with oracle tables on its own meta-block layout
/// reproduces the reference q11 stream byte for byte.
#[test]
fn captured_q11_parse_replays_exactly() {
    for (name, input) in fixtures() {
        let (reference, blocks) =
            capture_parse(&ReferenceParams::new(11, 22), &input, &[]).unwrap();
        let largest = blocks.iter().map(|b| b.len).max().unwrap_or(1);
        let block_bits = (32 - (largest.max(1) - 1).leading_zeros()).max(10);
        let opts = StreamOptions {
            block_bits,
            compact_ring: false,
            ..StreamOptions::external(22, 11)
        };
        let e = engine(opts);
        let mut ws = Workspace::default();
        let mut s = e.stream_with_parse(flatten_parse(&blocks));
        let mut out = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            let start = b.start as usize;
            s.feed(&mut ws, &input[start..start + b.len as usize], &mut out)
                .unwrap();
            // The reference marks its final data block ISLAST.
            if i + 1 < blocks.len() {
                s.cut(&mut ws, &mut out).unwrap();
            }
        }
        s.finish(&mut ws, &mut out).unwrap();
        assert_round_trip(&out, &input, name);
        assert_eq!(s.stats().rejected_copies, 0, "{name}");
        assert_eq!(s.stats().hints_ignored, 0, "{name}");
        let first = out.iter().zip(&reference).position(|(a, b)| a != b);
        assert!(
            out == reference,
            "{name}: replay {} B vs q11 {} B, first difference at {first:?}, blocks {:?}",
            out.len(),
            reference.len(),
            blocks.iter().map(|b| (b.start, b.len)).collect::<Vec<_>>()
        );
    }
}

#[test]
fn captured_parse_reinjects_per_flush() {
    for (name, input) in fixtures() {
        let (_, blocks) = capture_parse(&ReferenceParams::new(11, 22), &input, &[]).unwrap();
        let parse = flatten_parse(&blocks);
        if name == "prose" {
            assert!(
                parse
                    .iter()
                    .any(|c| matches!(c.reference, Reference::Dictionary { .. }) && c.copy_len > 0),
                "prose should exercise static-dictionary references"
            );
        }
        for table_quality in [2, 5, 11] {
            let opts = StreamOptions {
                block_bits: 12,
                ..StreamOptions::external(22, table_quality)
            };
            let e = engine(opts);
            let flushes: Vec<usize> = (1..input.len()).step_by(3001).collect();
            let (out, stats) = encode_checking_flushes(&e, Some(parse.clone()), &input, &flushes);
            let what = format!("{name} q{table_quality}");
            assert_round_trip(&out, &input, &what);
            assert_layout(&out, 12, &flushes, input.len(), &what);
            assert_eq!(stats.rejected_copies, 0, "{name} q{table_quality}");
            assert_eq!(stats.uncovered_bytes, 0);
        }
    }
}

/// Deliberately wrong commands are demoted to literals, never emitted.
#[test]
fn wrong_commands_are_caught() {
    let input = prose(30_000);
    let (_, blocks) = capture_parse(&ReferenceParams::new(11, 22), &input, &[]).unwrap();
    let parse = flatten_parse(&blocks);
    type Corruption = fn(&mut ExternalCommand, usize);
    let corruptions: [(&str, Corruption); 7] = [
        ("distance+1", |c, _| {
            if let Reference::Window { distance, .. } = &mut c.reference {
                *distance += 1;
            }
        }),
        ("distance past start", |c, i| {
            if let Reference::Window { distance, .. } = &mut c.reference {
                *distance = 1_000_000 + i as u32;
            }
        }),
        ("bogus short code", |c, i| {
            if let Reference::Window { short_code, .. } = &mut c.reference {
                *short_code = Some((i % 16) as u8);
            }
        }),
        ("dictionary word", |c, _| {
            if let Reference::Dictionary { word_idx, .. } = &mut c.reference {
                *word_idx ^= 1;
            }
        }),
        ("dictionary transform", |c, _| {
            if let Reference::Dictionary { transform, .. } = &mut c.reference {
                *transform = (*transform + 1) % 121;
            }
        }),
        ("copy one byte", |c, _| {
            if c.copy_len > 0 {
                c.copy_len = 1;
            }
        }),
        ("window as dictionary", |c, i| {
            if c.copy_len > 3 {
                c.reference = Reference::Dictionary {
                    word_len: 4 + (i % 8) as u8,
                    word_idx: i as u32 % 300,
                    transform: (i % 121) as u8,
                };
            }
        }),
    ];
    for (what, f) in corruptions {
        let bad: Vec<ExternalCommand> = parse
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut c = *c;
                if i % 3 == 0 {
                    f(&mut c, i);
                }
                c
            })
            .collect();
        for table_quality in [2, 11] {
            let e = engine(StreamOptions::external(22, table_quality));
            let (out, stats) = encode_with_flushes(
                &e,
                &mut Workspace::default(),
                Some(bad.clone()),
                &input,
                &[5000],
            )
            .unwrap();
            assert_round_trip(&out, &input, what);
            assert!(
                stats.rejected_copies + stats.hints_ignored > 0 || stats.uncovered_bytes > 0,
                "{what}: nothing caught"
            );
        }
    }
    // Lengths that do not match the input: too long, too short, garbage.
    let e = engine(StreamOptions::external(22, 11));
    for parse in [
        vec![ExternalCommand::window(10, 1_000_000, 1)],
        vec![ExternalCommand::literals(5)],
        vec![ExternalCommand::window(0, 50, 7); 10_000],
        Vec::new(),
    ] {
        let (out, _) = encode_with_flushes(
            &e,
            &mut Workspace::default(),
            Some(parse),
            &input,
            &[100, 20_000],
        )
        .unwrap();
        assert_round_trip(&out, &input, "lengths");
    }
}

#[test]
fn retained_cap_limits_distances() {
    // Two copies of a 100 KiB block 300 KiB apart: reachable at lgwin 22,
    // not with an 18-bit retained cap.
    let block = random(100_000);
    let mut input = block.clone();
    input.extend(templated(200_000));
    input.extend_from_slice(&block);
    for retained_bits in [18, 22] {
        let opts = StreamOptions {
            retained_bits,
            ..StreamOptions::cold(22)
        };
        let (out, stats) =
            encode_with_flushes(&engine(opts), &mut Workspace::default(), None, &input, &[])
                .unwrap();
        assert_round_trip(&out, &input, "cap");
        assert!(stats.max_distance <= (1 << retained_bits) - 16);
        if retained_bits == 22 {
            assert!(stats.max_distance >= 300_000, "far copy found");
        }
    }
    // An injected copy beyond the cap is rejected even though the declared
    // window would allow it.
    let mut parse = vec![ExternalCommand::literals(300_000)];
    parse.push(ExternalCommand::window(0, 100_000, 300_000));
    let opts = StreamOptions {
        retained_bits: 18,
        ..StreamOptions::external(22, 2)
    };
    let (out, stats) = encode_with_flushes(
        &engine(opts),
        &mut Workspace::default(),
        Some(parse),
        &input,
        &[],
    )
    .unwrap();
    assert_round_trip(&out, &input, "injected far copy");
    assert!(stats.rejected_copies > 0);
}

/// A copy exactly at the history limit is emitted; one byte farther is
/// rejected, for both ring layouts.
#[test]
fn history_limit_is_exact() {
    for compact_ring in [true, false] {
        let opts = StreamOptions {
            retained_bits: 12,
            block_bits: 10,
            compact_ring,
            ..StreamOptions::external(12, 2)
        };
        let limit = ExternalOptions {
            lgwin: opts.lgwin,
            retained_bits: opts.retained_bits,
            block_bits: opts.block_bits,
            table_quality: opts.table_quality,
            search_quality: opts.search_quality,
            compact_ring,
        }
        .max_backward() as usize;
        assert_eq!(limit, if compact_ring { 3056 } else { 4080 });
        for (d, ok) in [(limit, true), (limit + 1, false)] {
            let r = random(1000);
            let mut input = r.clone();
            input.extend(templated(d - 1000));
            input.extend_from_slice(&r);
            let parse = vec![
                ExternalCommand::literals(d as u32),
                ExternalCommand::window(0, 1000, d as u32),
            ];
            let (out, stats) = encode_with_flushes(
                &engine(opts),
                &mut Workspace::default(),
                Some(parse),
                &input,
                &[],
            )
            .unwrap();
            assert_round_trip(&out, &input, "limit");
            assert_eq!(
                stats.rejected_copies == 0,
                ok,
                "compact {compact_ring} d {d}"
            );
        }
    }
}

#[test]
fn bad_options_are_rejected() {
    for opts in [
        StreamOptions {
            lgwin: 25,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            retained_bits: 23,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            search_quality: 10,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            table_quality: 1,
            ..StreamOptions::cold(22)
        },
        StreamOptions {
            retained_bits: 16,
            block_bits: 16,
            ..StreamOptions::cold(22)
        },
    ] {
        assert!(Engine::new(opts).is_err(), "{opts:?}");
    }
}

/// Feed `input` split at `cuts`, flushing at each absolute offset in `flushes`.
fn encode_fragmented(e: &Engine, input: &[u8], cuts: &[usize], flushes: &[usize]) -> Vec<u8> {
    let mut points: Vec<(usize, bool)> = cuts.iter().map(|&c| (c, false)).collect();
    points.extend(flushes.iter().map(|&f| (f, true)));
    points.sort_unstable();
    let mut ws = Workspace::default();
    let mut s = e.stream();
    let mut out = Vec::new();
    let mut pos = 0;
    for (p, is_flush) in points {
        let p = p.min(input.len());
        s.feed(&mut ws, &input[pos..p], &mut out).unwrap();
        pos = p;
        if is_flush {
            s.flush(&mut ws, &mut out).unwrap();
        }
    }
    s.feed(&mut ws, &input[pos..], &mut out).unwrap();
    s.finish(&mut ws, &mut out).unwrap();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    /// Feed fragmentation never changes the output for fixed logical
    /// flushes, and the output always decodes exactly.
    #[test]
    fn fragmentation_does_not_change_output(
        which in 0usize..4,
        len in 0usize..12_000,
        mut flushes in proptest::collection::vec(0usize..12_000, 0..6),
        cuts_a in proptest::collection::vec(0usize..12_000, 0..20),
        cuts_b in proptest::collection::vec(0usize..12_000, 0..20),
        block_bits in 10u32..13,
        table_quality in prop_oneof![Just(2u32), Just(5), Just(11)],
    ) {
        let input = match which {
            0 => templated(len),
            1 => runs(len),
            2 => mixed(len.max(2000)),
            _ => prose(len),
        };
        flushes.sort_unstable();
        let opts = StreamOptions { block_bits, table_quality, ..StreamOptions::cold(20) };
        let e = engine(opts);
        let a = encode_fragmented(&e, &input, &cuts_a, &flushes);
        let b = encode_fragmented(&e, &input, &cuts_b, &flushes);
        prop_assert!(a == b, "fragmentation changed output");
        let (c, _) = encode_with_flushes(&e, &mut Workspace::default(), None, &input, &flushes).unwrap();
        prop_assert!(a == c, "differs from one-shot feed");
        prop_assert!(brotli_c::decode(&a, None).unwrap() == input);
    }

    /// Arbitrary command lists never produce wrong bytes.
    #[test]
    fn arbitrary_commands_never_corrupt(
        data in proptest::collection::vec(prop_oneof![Just(b'a'), Just(b'b'), any::<u8>()], 0..3000),
        raw in proptest::collection::vec((0u32..400, 0u32..400, 0u32..3000, any::<bool>(), 0u8..20), 0..60),
        flushes in proptest::collection::vec(0usize..3000, 0..4),
    ) {
        let parse: Vec<ExternalCommand> = raw
            .into_iter()
            .map(|(insert_len, copy_len, d, dict, code)| ExternalCommand {
                insert_len,
                copy_len,
                reference: if dict {
                    Reference::Dictionary { word_len: (code % 26), word_idx: d, transform: code * 7 }
                } else {
                    Reference::Window { distance: d, short_code: (code < 16).then_some(code) }
                },
            })
            .collect();
        let mut flushes = flushes;
        flushes.sort_unstable();
        for tq in [2u32, 11] {
            let opts = StreamOptions { block_bits: 10, ..StreamOptions::external(16, tq) };
            let (out, _) = encode_with_flushes(&engine(opts), &mut Workspace::default(), Some(parse.clone()), &data, &flushes).unwrap();
            prop_assert!(brotli_c::decode(&out, None).unwrap() == data);
            prop_assert!(decode_rust(&out) == data);
        }
    }
}
