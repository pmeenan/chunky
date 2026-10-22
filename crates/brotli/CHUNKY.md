# chunky fork of rust-brotli

- Upstream: https://github.com/dropbox/rust-brotli at `6518a4e`
  (9.0.0, 2026-09-02). BSD-3-Clause AND MIT; upstream notices kept in
  `LICENSE.BSD-3-Clause` and `LICENSE.MIT`.
- Fork mechanics: D-020 (external-command entry point, `portable-float` on
  for every measured or shipped build) and D-023 (layout). D-023 names a git
  subtree; the tree is kept as a plain copy (minus `.github/`) instead
  (D-028), so this file is the record of what changed on top.
- Changes on top of upstream (keep each one upstreamable):
  - `Cargo.toml`: drop `[profile.release]` (the workspace root sets
    profiles) and allow clippy crate-wide; fork additions opt back in with
    `#![warn(clippy::all)]` in their own modules.
  - `src/enc/test.rs`: allow two newer rustc lints in upstream test helpers.
  - `src/enc/backward_references/mod.rs` (RE-009): `StoreRangeOptBasic`
    and `StoreRangeOptBatch` store the unmasked position in the hash
    buckets, like `Store` and C, instead of `ix & mask`. The masked value
    made bulk-stored entries unreachable after the ring buffer wrapped, so
    q2–q5 lost 9–20% on inputs over 2^(lgwin+1). Upstreamable as-is; offer
    it to dropbox/rust-brotli.
