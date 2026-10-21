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
  (D-008). In the Worker deployment, runtime shared state is immutable per
  versioned model blob so cross-isolate determinism holds by construction
  (D-011).
- **Admission gate is the only route into shared state** (D-009), and upgrade
  parses run over composite sequences of admitted chunks with chunk-bounded
  references (D-010).
- **Corpus lives outside the repo** (D-007).
- **Per-stream window memory is demand-paged and dictionaries are zero-copy**
  so concurrent streaming requests fit within the 128 MB isolate cap (D-011).

## Expected shape (to be validated in the M0 draft)

Where a bullet below leans on a `proposed` features.md row, it is a design
assumption to confirm during feature triage, not settled scope.

### Workspace

A sketch for M1 to confirm or rename:

| Crate / dir | Role | Targets |
| --- | --- | --- |
| `brotli` fork (vendored or git dependency; open question 2) | Entropy coder + meta-block writer with command-list injection (fast single-table + q11 oracle modes); reference q1/q11 encoder | native, wasm32 |
| `chunky-core` | Chunker, parse cache, stitcher, fast hasher, route model, table build, streaming layout, mode selection, admission gate. Demand-grown window buffers and zero-copy dictionary references. No I/O, threads or clocks: time budgets and storage come in through traits | native, wasm32 |
| `chunky-formats` | Versioned binary formats: cache entries, model blob, warm cache set, dictionary sidecar. Header plus version check; mismatch falls back to the generic path | native, wasm32 |
| `chunky-cli` (one binary with subcommands, or several bins) | crawl, label-routes, baseline, overlap, encode, train, verify, inspect, replay, bench, and (M8) sidecar | native |
| `worker/` | Cloudflare Worker: request handling, origin fetch, streaming compress, state loading | wasm32 (+ JS shell if chosen) |

`chunky-core` stays `#![no_std]`-friendly, or at least free of `std::fs`,
threads and `Instant`, so the same code runs in the CLIs and the Worker. The
CLIs supply native implementations of the I/O traits, and the Worker supplies
Workers-runtime ones.

### Memory budget and window management in wasm (D-011)

A single Worker isolate handles many concurrent streaming requests overlapped on
origin network I/O, and wasm linear memory (`memory.grow`) never shrinks for the
life of the isolate. To stay well under the 128 MB isolate limit:

- **Shared immutable state:** One isolate-global wasm instance holds the loaded
  model blob, warm parse cache and any retained CDT dictionary + sidecar once
  per isolate, shared read-only across all requests in that isolate.
- **Demand-grown per-stream window buffers:** Logical `lgwin` (typically 22, or
  up to 24 with CDT) is decoupled from physical buffer allocation. A stream's
  window slab starts small (e.g. 64 KB for HTML/JSON) and grows on demand or
  draws from a bounded pool, rather than eagerly allocating `1 << lgwin` (4 to
  16 MB) per active encoder.
- **Zero-copy CDT dictionaries:** Negative window offsets (`< 0`) read directly
  from the shared immutable dictionary slice rather than copying dictionary
  bytes into each stream's ring buffer.

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
   the Worker may see a normalized `Accept-Encoding`), and the response is
   eligible (status 200, compressible content type, not a `206 Partial Content`
   or an un-decoded custom encoding such as `dcb`/`dcz`). Note that when an
   origin sends `Content-Encoding: gzip` or `br`, Cloudflare's `fetch()`
   transparently decompresses `originResponse.body` while leaving the origin's
   `Content-Encoding` and compressed `Content-Length` headers on
   `originResponse.headers`. Treat `Content-Length` as the known uncompressed
   `MLEN` *only* when `originResponse.headers.get("content-encoding")` is
   absent; otherwise `Content-Length` only indicates that the origin buffered
   the response before sending headers (D-011).
3. Prepare outgoing headers: set `Content-Encoding: br`, merge `Vary:
   Accept-Encoding`, strip `Content-Length` (and any origin template-hint
   headers), and convert any strong `ETag` (`"xyz"`) to weak (`W/"xyz"`) per
   RFC 9110 §8.8.3. Return `new Response(readable, { encodeBody: "manual", ... })`
   from a `TransformStream` immediately.
4. Compression runs inside `ctx.waitUntil(...)`. Each origin `reader.read()`
   feeds the encoder. Streaming-critical responses flush per read (with
   same-tick coalescing), and completion-bound ones buffer to a cap. On any
   encoder error, budget overrun or format version mismatch, fall back safely
   before bytes are committed (or via reference q1).
5. Finalization runs after the last byte inside `waitUntil` and only records
   telemetry / chunk-frequency counters / out-of-band samples. Runtime shared
   state inside the isolate is never mutated in place (D-011).

dictionary-worker's brotli path noted that the stock encoder "doesn't handle
streaming incremental chunks" (it flushes when it decides to). Our encoder
owns its flush decisions, which is the point of the streaming layout.

### Where the learning tiers run on Workers (D-011, open Q4)

| Design tier | Native (CLI / future server) | Worker |
| --- | --- | --- |
| Inline | In the encoder (reads only) | In the encoder (reads immutable versioned blob only) |
| Post-stream finalization | After last byte | After last byte inside `waitUntil`; isolate-local counters / out-of-band telemetry |
| Async upgrade queue | Background thread or offline | Folded into the offline trainer so isolates stay deterministic and within CPU limits |
| Offline fleet model | `chunky train` CLI | Same CLI; publishes a new versioned model + warm cache blob to Workers storage |

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
