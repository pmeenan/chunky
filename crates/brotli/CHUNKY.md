# chunky fork of rust-brotli

- Upstream: https://github.com/dropbox/rust-brotli at `6518a4e`
  (9.0.0, 2026-09-02). BSD-3-Clause AND MIT; upstream notices kept in
  `LICENSE.BSD-3-Clause` and `LICENSE.MIT`.
- Fork mechanics: D-020 (external-command writer, `portable-float` on for
  every measured or shipped build) and D-023 (layout). D-023 names a git
  subtree; the tree is kept as a plain copy (minus `.github/`) instead
  (D-028), so this file is the record of what changed on top.
- C google/brotli is the comparison point (D-016); rust-brotli is a means to
  an end. Where the port diverges from C, fix the fork toward C and list the
  fix below as an upstream candidate.

## Upstream candidates

Bug fixes that move rust-brotli toward C. Each is self-contained and
upstreamable as-is; none has been offered to dropbox/rust-brotli yet. Before
offering one, recheck that upstream master still has the bug (last checked
2026-10-22: master was still `6518a4e`, so both are present).

| Fix | Files | Effect | Regression test | Status |
| --- | --- | --- | --- | --- |
| Bulk hash stores keep unmasked positions (RE-009) | `src/enc/backward_references/mod.rs`: `StoreRangeOptBasic`, `StoreRangeOptBatch` | Stored `ix & mask`, like no other path and unlike C; after the ring buffer wrapped those entries read as too far back, so q2–q5 lost 9–20% on inputs over 2^(lgwin+1) | `chunky-core` `reference::tests::tracks_c_after_ring_buffer_wrap` | not offered |
| Context-mode UTF-8 test reads the data, not the allocation start (RE-011) | `src/enc/encode.rs` `encode_data`: `ChooseContextMode` gets `data_mo[buffer_index..]` | Passed `data_mo` without the 2-byte `buffer_index`, so q10/q11 judged UTF-8 on a window two bytes early (C passes `ringbuffer_.buffer_`). Changes the literal context mode only at the 75% threshold; no corpus page changed at q10/q11 | `chunky-core` `reference::tests::literal_context_mode_matches_c_at_the_utf8_threshold` | not offered |

Known, not yet investigated: rust q10/q11 output is 0.1–0.4% larger than C
v1.2.1 on the M1 corpus (rust/C 1.0041 buffered, 1.0024 served, 1.0009 per
read at q11; `chunky baseline <sessions> --engines c,rust --qualities 10,11`).

## Chunky-only changes

Not meant for upstream as they stand.

- `Cargo.toml`: drop `[profile.release]` (the workspace root sets
  profiles) and allow clippy crate-wide; fork additions opt back in with
  `#![warn(clippy::all)]` in their own modules.
- `src/enc/test.rs`: allow two newer rustc lints in upstream test helpers.
- `src/enc/external.rs` (new, `std` only; M2, D-020): `ExternalEncoder`,
  a per-flush meta-block writer over a ring sized from a retained-history
  cap instead of the declared window (D-017). It writes one actual-length
  meta-block per call from caller-supplied logical commands
  (`ExternalCommand`: insert length, copy length and a window distance or
  a static-dictionary word/transform) or from its own q2–q9 search over
  retained history (the cold path). Every copy is compared against the
  retained bytes and every dictionary word is expanded and compared before
  the existing `WriteMetaBlockInternal` sees it; failures become literals.
  Dictionary transforms producing fewer than two bytes become literals too,
  preserving the native writer's minimum-copy-length invariant (RE-012).
  Short codes and dictionary wire distances are resolved at emit time; an
  optional per-command code hint (a short code, or `EXPLICIT_DISTANCE`)
  makes captured parses replay exactly. With `search_quality` 0 no hasher
  is allocated. By default the ring is compact: `1 << retained_bits` holds
  the history plus one open block (reach `(1 << retained_bits) - (1 <<
  block_bits) - 16`), half of upstream's `2 << lgwin` layout, which
  `compact_ring: false` keeps. Block scratch (command array, bit-writer
  buffer, histogram and table-construction temporaries) lives in a separate
  `Workspace` passed to each call, so one per thread serves many streams
  and the encoder holds only per-stream state (D-029). D-020 sketched a single `encode_external_metablock`
  function on the reference state; a separate struct keeps the reference
  encoder untouched and per-stream state small (D-025).
- `src/enc/backward_references/mod.rs`: `BrotliEncoderParams` gains
  `max_backward_distance` (0 = derive from `lgwin`, as upstream). The H2–H9
  search (`CreateBackwardReferences`) stops there, so the cold path searches
  exactly the history a compact ring retains.
- `src/enc/encode.rs`: helpers `external.rs` needs are `pub(crate)`
  (ring buffer init/write, `WriteMetaBlockInternal`, context-mode and
  distance-parameter choice, hasher stitch/reset, `WrapPosition`). New
  `enable_parse_log`/`take_parse_log` record each meta-block's commands,
  with their distance codes, just before `WriteMetaBlockInternal`
  (`ParseLog`); output is unchanged. `RingBufferWrite` skips its small
  first-write buffer when the ring already holds a full-size one, so
  `ExternalEncoder::reset` can reuse it in place (upstream never reaches
  position 0 with a full buffer, so its behavior is unchanged).
