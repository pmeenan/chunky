# chunky

HTTP-optimized stream compression: a learned brotli encoder for dynamic
responses.

chunky is a research project exploring whether a brotli encoder can produce
q11-class output at close to q1 CPU on templated dynamic HTML and JSON. It
remembers parse and modeling decisions across responses instead of
recomputing them per stream. The full technical design is in
[docs/design.md](docs/design.md).

- **Standard output.** Ordinary responses use brotli (RFC 7932); optional CDT
  variants use negotiated `dcb` (RFC 9841/9842). Decoding preserves every input
  byte, with no custom client changes.
- **Memoized encoding.** A content-defined parse cache and pre-trained
  per-route entropy models replace the expensive parts of q11.
- **Streaming-aware.** Per-flush meta-blocks preserve prompt delivery while
  learned tables reduce header cost. A single-meta-block optimization is a
  later experiment, gated on exact lengths and measured delivery behavior.
- **Rust, native and wasm.** One codebase builds CLI tools for crawling,
  measuring and training, and a Cloudflare Worker that runs the encoder as a
  compressing proxy.

What else is in or out of scope is being triaged in
[docs/features.md](docs/features.md).

## Status

**M2 (command injection and per-flush output) is done; M3 is next.**
The Cargo workspace, the pinned C and rust-brotli reference encoders, corpus
tooling and the M1 baselines exist. M2 added the encoder's verified
per-flush writer with a q2-class cold path, parse capture and replay, and an
inspector (`chunky inspect`). See [docs/plan.md](docs/plan.md) and
[docs/m1-report.md](docs/m1-report.md).

## Building and checking

Host prerequisites (D-023): Rust via rustup (the toolchain is pinned in
`rust-toolchain.toml`), `clang` and `lld`, wasi-sdk 34 under `~/.local` (for
the C wasm32 baseline), Node 24, and `cargo install --locked cargo-deny`.

```bash
scripts/check.sh
```

runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, the wasm32 build of `chunky-wasm` and the
fork, Worker stream/identity regression tests, `cargo deny check licenses bans`,
and syntax checks on `tools/` and `worker/`. On a shared measurement host, wrap it:
`hostlock shared --label "chunky: check" -- scripts/check.sh`.

The `chunky` CLI (`cargo build --release -p chunky-cli`) holds the
measurement tools: `flushes`, `baseline`, `summarize`, `reach`, `mem`,
`parse-stats`, `funnel`, `generic-reuse` and `bench` (which takes `hostlock
exclusive` itself).
[docs/m1-report.md](docs/m1-report.md) lists the exact commands.

## How it's built

Almost all code is written by AI agents working from the project
documentation, directed and reviewed by a human who gates every commit. See
[docs/workflow.md](docs/workflow.md).

## Start here

- [AGENTS.md](AGENTS.md): project rules and doc map
- [docs/design.md](docs/design.md): technical design
- [docs/vision.md](docs/vision.md): goals, success criteria, non-goals
- [docs/features.md](docs/features.md): scope ledger and open questions
- [docs/plan.md](docs/plan.md): milestones
- [docs/workflow.md](docs/workflow.md): how changes are made
- [docs/rough-edges.md](docs/rough-edges.md): findings log

## License

Apache-2.0. See [LICENSE](LICENSE).
