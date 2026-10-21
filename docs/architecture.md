# Architecture

> **Status: skeleton.** The first full draft is an M0 exit criterion. What is
> written here now is the load-bearing shape already settled, so drafting can
> build on it rather than re-derive it. The *encoder* architecture is in
> [design.md](design.md). This doc covers how that design maps onto code,
> tools and the Worker deployment.

## Fixed points (from decisions)

- **Output is standard brotli** (D-002). Every path that produces bytes for a
  client is round-trip tested against an independent decoder.
- **design.md is the baseline** (D-001). The hot-path pipeline is chunker →
  cache lookup → resolve references → flush (count + tables + meta-block) →
  finalization. Shared state is the parse cache, dictionary sidecars and route
  model. Learning tiers are inline counters, post-stream finalization, the
  upgrade queue and the offline fleet model.
- **Rust core, CLIs and wasm in one Cargo workspace; encoder forked from
  rust-brotli** (D-003).
- **Server deployment is a Cloudflare Worker compressing proxy** (D-004). It
  uses `encodeBody: "manual"` and stays within 128 MB per isolate including
  wasm memory. Per-request CPU budget depends on the plan.
- **Hot path reads; learning writes off-request; the cold path is valid**
  (D-008).
- **Admission gate is the only route into shared state** (D-009).
- **Corpus lives outside the repo** (D-007).

## Expected shape (to be validated in the M0 draft)

Where a bullet below leans on a `proposed` features.md row, it is a design
assumption to confirm during feature triage, not settled scope.

### Workspace

A sketch for M1 to confirm or rename:

| Crate / dir | Role | Targets |
| --- | --- | --- |
| `brotli` fork (vendored or git dependency; open question 2) | Entropy coder + meta-block writer with command-list injection; reference q1/q11 encoder | native, wasm32 |
| `chunky-core` | Chunker, parse cache, stitcher, fast hasher, route model, table build, streaming layout, mode selection, admission gate. No I/O, threads or clocks: time budgets and storage come in through traits | native, wasm32 |
| `chunky-formats` | Versioned binary formats: cache entries, model blob, warm cache set, dictionary sidecar. Header plus version check; mismatch falls back to the generic path | native, wasm32 |
| `chunky-cli` (one binary with subcommands, or several bins) | crawl, baseline, overlap, encode, train, and, pending triage, verify, inspect, replay, label-routes, sidecar, bench | native |
| `worker/` | Cloudflare Worker: request handling, origin fetch, streaming compress, state loading | wasm32 (+ JS shell if chosen) |

`chunky-core` stays `#![no_std]`-friendly, or at least free of `std::fs`,
threads and `Instant`, so the same code runs in the CLIs and the Worker. The
CLIs supply native implementations of the I/O traits, and the Worker supplies
Workers-runtime ones.

### Data flow between tools and the Worker

```
crawl ──► corpus (local, git-ignored) ──► baseline / overlap / encode / bench
                                     └──► train ──► model blob + warm cache set
                                                         │  (versioned format)
                                                         ▼
                              Worker loads at isolate start (storage: open Q4)
```

The offline trainer CLI is the design's "offline fleet model". Running it
against the corpus and shipping its output to the Worker is how cross-instance
determinism holds: every isolate loads the same versioned blob.

### Worker request path

Patterned on the owner's
[dictionary-worker](https://github.com/pmeenan/dictionary-worker), which
already does transparent streaming compression on Workers:

1. Request arrives. Kick off lazy, isolate-global wasm and model init *in
   parallel* with the origin `fetch` (dictionary-worker's `wasmLoaded`
   promise pattern) so cold starts overlap origin latency.
2. Negotiate: client accepts `br` (check `request.cf.clientAcceptEncoding`;
   the Worker may see a normalized `Accept-Encoding`), the response is
   eligible (status, content type, not already encoded), and the mode comes
   from headers (design: Response modes).
3. Return `new Response(readable, { encodeBody: "manual", ... })` from a
   `TransformStream` immediately, with `Content-Encoding: br` and `Vary`.
4. Compression runs inside `ctx.waitUntil(...)`. Each origin `reader.read()`
   feeds the encoder. Streaming-critical responses flush per read (with
   same-tick coalescing), and completion-bound ones buffer to a cap.
5. Finalization records into isolate-local accumulators after the last byte.
   Anything shared is written via `waitUntil` to the store chosen in open
   question 4, never on the response path.

dictionary-worker's brotli path noted that the stock encoder "doesn't handle
streaming incremental chunks" (it flushes when it decides to). Our encoder
owns its flush decisions, which is the point of the streaming layout.

### Where the learning tiers run on Workers (assumption, open Q4)

| Design tier | Native (CLI / future server) | Worker |
| --- | --- | --- |
| Inline | In the encoder | In the encoder |
| Post-stream finalization | After last byte | After last byte inside `waitUntil`; isolate-local accumulators |
| Async upgrade queue | Background thread | Not available in-isolate. Candidates: Queues consumer, a Durable Object, or left to the offline trainer |
| Offline fleet model | `chunky train` CLI | Same CLI; output published to Workers storage |

## Open architecture questions

The ranked list is in [features.md](features.md#open-questions-answer-during-m0).
The ones this doc must answer before its first full draft:

- Workers state placement and how the upgrade tier runs there (Q4).
- JS shell vs workers-rs, and the wasm/JS boundary for streaming chunks:
  copy-in/copy-out buffers as in dictionary-worker, or a shared-memory view
  (Q7).
- rust-brotli fork mechanics and where command injection plugs in (Q2).
- The I/O trait surface of `chunky-core` (time budget, storage, randomness if
  any), designed so determinism (same input + same model version → same bytes)
  holds across native and wasm.
- How the model blob reaches isolates: bundled in the Worker, static assets
  binding (dictionary-worker served dictionaries this way), R2, or KV. This
  bounds how fast models can update relative to deploys.
