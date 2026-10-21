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

**Pre-code, milestone M0 (planning).** No code exists yet. The first
measurement milestone measures reuse on later responses held out from training,
against matched q1/q4/q11 baselines. See [docs/plan.md](docs/plan.md).

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
