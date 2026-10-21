# chunky

HTTP-optimized stream compression: a learned brotli encoder for dynamic
responses.

chunky is a research project exploring whether a brotli encoder can produce
q11-class output at close to q1 CPU on templated dynamic HTML and JSON. It
remembers parse and modeling decisions across responses instead of
recomputing them per stream. The full technical design is in
[docs/design.md](docs/design.md).

- **Standard output.** It emits ordinary brotli (RFC 7932) that any current
  browser decodes, with no new content-encoding and no client changes.
- **Memoized encoding.** A content-defined parse cache and pre-trained
  per-route entropy models replace the expensive parts of q11.
- **Streaming-aware.** It is designed for servers that flush on every write,
  without per-flush header overhead.
- **Rust, native and wasm.** One codebase builds CLI tools for crawling,
  measuring and training, and a Cloudflare Worker that runs the encoder as a
  compressing proxy.

What else is in or out of scope is being triaged in
[docs/features.md](docs/features.md).

## Status

**Pre-code, milestone M0 (planning).** No code exists yet. The first
measurement milestone checks whether real templated sites repeat enough bytes
per route for the design to pay off. See [docs/plan.md](docs/plan.md).

## How it's built

Almost all code is written by AI agents working from the project
documentation, directed and reviewed by a human who makes every commit. See
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
