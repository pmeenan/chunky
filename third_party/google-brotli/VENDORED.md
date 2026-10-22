# Vendored: google/brotli v1.2.1

- Upstream: https://github.com/google/brotli, tag `v1.2.1`, commit
  `db24a98aed38a16de78cc05ca15cdd8dc2a0d92d` (2026-10-21). MIT, see LICENSE.
- Role (D-016, D-023): the pinned C baseline encoder for the reference
  quality curve and the independent reference decoder. Built natively by
  `crates/brotli-c` (clang `-O2 -DNDEBUG`, no `-march`) and to wasm32 by
  `crates/brotli-c/wasm/build.sh` (wasi-sdk 34).
- Contents: `c/`, `LICENSE`, `README.md` and `CHANGELOG.md` from the tag,
  unmodified. The other language bindings, tests and build files are left
  out. They are kept as a plain copy rather than D-023's git subtree
  (D-028); `git subtree add --prefix third_party/google-brotli
  https://github.com/google/brotli v1.2.1 --squash` reproduces the full tree
  if history is ever wanted.
