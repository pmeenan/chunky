# Feature matrix

This is the scope ledger for the M0 planning conversations. It has three tiers:

- **Confirmed**: stated project scope, from [design.md](design.md) (adopted by
  D-001) or the owner's kickoff choices. Milestone assignment happens in
  [plan.md](plan.md) as the plan firms up.
- **Proposed**: candidate additions awaiting a yes/no from the project owner.
- **Open questions**: things that shape architecture and need an answer
  during M0.

Status legend: `confirmed` · `proposed` · `rejected (D-NNN)`

Confirmed rows describe *what* is in scope. The design doc holds the *how*, and
the section to read is named in Notes.

## Encoder core (library, wasm-compatible)

| Feature | Status | Notes |
| --- | --- | --- |
| Command-list injection: forked rust-brotli entropy coder and meta-block writer accept an externally produced command list (supporting both fast single-table coding and q11 oracle table coding) | confirmed | design: Format choice, Milestone 2. The primitive everything else hangs off. Validate by round-tripping q11's own parse; oracle table mode lets M3 isolate parse-cache gains before M4 lands |
| Content-defined multi-resolution chunker (~256 B fine, ~4 KB coarse levels from one rolling hash, with optional delimiter/tag bias) | confirmed | design: Parse cache, Chunker resolution. Parameters are a versioned, deployment-wide constant |
| Parse cache: chunk fingerprint → commands with strictly chunk-contained `chunk-anchored`, `dictionary-anchored` and `predicted-distance` logical references; prefix index; stitcher resolving distance short codes (`0..15`) and literal contexts (`p1`, `p2`) at emit time | confirmed | design: Parse cache; D-010 |
| Slot handling: prefix extension to divergence, learned cut rules | confirmed | design: Parse cache → Slots |
| q1-class fast hasher for misses, with predicted-distance hints and a time cap | confirmed | design: Parse cache → Misses |
| Route model: pre-trained context map, block-type policy, priors, table ladder, size distributions | confirmed | design: Entropy and modeling state |
| Prior-smoothed Huffman tables built per meta-block (pruned for known-block modes; unpruned universal tables for one-meta-block streaming), small-block size policy | confirmed | design: Entropy and modeling state; D-010 |
| Streaming layout: one meta-block per response with committed MLEN, `\n` + `distance=1` shortfall padding, copy-ending mid-meta-block flushes, byte-alignment lottery (with 2–4 B tail-copy split fallback), small-first-flush | confirmed | design: Streaming; D-010. **Gated on the decoder partial-output verification** (open question 1); `text/event-stream` stays on per-flush fallback |
| Per-flush meta-block fallback with quantized table ladder | confirmed | design: Streaming → fallback |
| Two-axis response-mode selection from headers (size known × streaming matters), checking unencoded `Content-Length` | confirmed | design: Response modes; D-011 |
| Runtime content scanning: base64/data URIs, embedded sublanguages, high-entropy tokens get dedicated block types / tables | confirmed | design: Content-aware optimizations |
| Template hints: slot boundaries, slot types, secret slots | confirmed | design: Content-aware optimizations. How hints reach the encoder is open question 6 |
| Admission gate and secret-slot policy | confirmed | D-009 |
| Cold-cache / generic-model path produces valid q1-or-better output | confirmed | D-008. First-class test configuration |
| Deterministic output for identical input + identical model/cache version | confirmed | design: Learning architecture, Security → Determinism; D-011 |
| Demand-grown per-stream window buffers and zero-copy CDT dictionary slices | confirmed | D-011; keeps concurrent streaming requests within the 128 MB Worker isolate limit |

## Learning and models

| Feature | Status | Notes |
| --- | --- | --- |
| Post-stream finalization: fingerprint sequences, admission-threshold copies, accumulators, sampling | confirmed | design: Learning architecture |
| Upgrade path: q11-quality parses for admitted chunks against composite admitted-chunk route windows (commands clamped/split at chunk boundaries), computed off-request | confirmed | design: Learning architecture; D-010, D-011 |
| Offline fleet-model trainer producing a versioned model blob + warm cache set | confirmed | design: Learning architecture. Natural fit for a CLI tool |
| Versioned on-disk formats for model blobs, cache sets and sidecars, with a header/version check on load | confirmed | Published state between the CLI trainer and the Worker; a version mismatch fails closed to the generic path, never mis-decodes |

## Dictionaries and CDT (secondary target)

| Feature | Status | Notes |
| --- | --- | --- |
| Dictionary sidecar (chunk index, prefix index, stats, geometry, cut rules) | confirmed | design: Dictionaries and CDT → sidecar. "mmap" becomes "load a blob" in the Worker |
| CDT static-asset variants: chunk dedup, localized search, parse recovery from build `.br` via a command-emitting decoder (resolving short codes and static-dict offsets) | confirmed | design: Static asset variants; D-010. The modified decoder is offline-only (D-002) |
| Generated site dictionary from admitted chunks, served via `Use-As-Dictionary` | confirmed | design: Generated site dictionary |

## CLI tools (build and test)

The owner's kickoff ask: "a collection of CLI tools for building and testing
it." Load-bearing measurement and verification tools are confirmed; later-stage
or optional tools remain proposed or scoped to later milestones.

| Feature | Status | Notes |
| --- | --- | --- |
| Corpus crawler/collector: logged-out polite crawl, stores bodies + headers + route labels in the local data dir | confirmed | D-007; design: Milestone 1 |
| Route-labeling tool: cluster crawled URLs into routes (path templates) | confirmed | Third-party crawls carry no route ids (D-007); every per-route metric in M1+ depends on this |
| Baseline tool: reference q1 and q11 size and CPU per response, per route | confirmed | design: Milestone 1. Cross-check once against C `google/brotli` (D-003) |
| Chunk-overlap analyzer: 4-step repeatability funnel per route (cross-response LZ77 + static dict ceiling, naive ~256 B CDC, 64/128/256 B + delimiter-biased CDC, and ~256 B CDC with prefix/suffix extension + simulated cut rules) | confirmed | design: "The first experiment". Separates true template repetition from chunker/slot recovery; go/no-go gate |
| Encoder CLI: compress a file or corpus with a given cache/model, report size, CPU and mechanism breakdown | confirmed | Needed for milestones 3 to 5 measurements |
| Model/cache trainer CLI (the offline fleet model) | confirmed | Same as the Learning row above, surfaced as a tool |
| Round-trip verifier: decode every output with an independent decoder (brotli-decompressor and the C reference) and byte-compare | confirmed | Makes D-002 mechanical from M2 onward |
| Stream inspector: disassemble a brotli stream into meta-blocks, commands, table description costs and flush/alignment points | confirmed | Needed to measure header cost, alignment lottery and padding in M4–M5 |
| Flush replay harness: replay a recorded response as a flush-on-every-write stream with recorded timing | confirmed | Required by design milestone 5 ("flush-on-every-write replay") |
| Benchmark report generator: one command → a comparable table (vs q1/q11) per corpus snapshot | confirmed | Rule 3 says claims are numbers; keeps M1–M9 tables reproducible |
| Differential fuzzing / property tests: random inputs and cache states → round-trip must hold | confirmed | Property tests start in M2; full fuzzing in M3/M5 where stitched anchored refs and committed-MLEN padding live |
| Sidecar builder CLI for CDT dictionaries | confirmed | Packaging of the confirmed sidecar feature for M8 |

## Cloudflare Worker (compressing proxy)

| Feature | Status | Notes |
| --- | --- | --- |
| Worker proxy: fetch from origin, compress eligible responses with the wasm core, return with `encodeBody: "manual"`, strip `Content-Length` and weaken strong `ETag`s | confirmed | D-004, D-011. Prior art: the owner's [dictionary-worker](https://github.com/pmeenan/dictionary-worker) (Apache-2.0); see [architecture.md](architecture.md) |
| Content negotiation: only send `br` when the client accepts it; `Vary: Accept-Encoding` | confirmed | Correctness requirement, not a scope choice. Prior art reads `request.cf.clientAcceptEncoding` because the Worker may not see the client's original header |
| Streaming pass-through: compress origin reads as they arrive, emitting flushes without buffering streaming-critical responses | confirmed | design: Streaming, Zero-latency coalescing. Edge behavior is open question 3 |
| Mode selection from request/response headers (Sec-Fetch-Dest, Priority, unencoded Content-Length, Content-Type, Cache-Control) | confirmed | design: Response modes; D-011 |
| Load immutable versioned model blob + warm cache set at isolate start | confirmed | design: fleet model; D-011 |
| Safe fallback: on any encoder error, budget overrun or version mismatch, serve plain brotli (reference q1) or pass through uncompressed | confirmed | Keeps the proxy safe while the encoder is experimental |
| Strip origin-supplied template-hint headers before the response reaches the client | confirmed | Lets a cooperating origin pass hints via response headers at zero client wire cost (open question 6) |
| Shadow / comparison mode: also compute reference q1 size off-path and log the delta | proposed | Optional M6+ diagnostic; gives in-production size numbers without client-visible risk |
| Debug response header (opt-in) reporting cache hit rate, mode, bytes saved | proposed | Optional M6+ diagnostic; must be off by default (it leaks cache state; D-009) |
| Metrics via Workers Analytics Engine or logs: hit rate, CPU per response, MLEN overflow/padding rates | proposed | Optional M6+ telemetry for production-side counters |
| Kill switch / per-route enable list | proposed | Optional M6+ operational control |
| Cache API storage of compressed output for cacheable (`public`) responses | proposed | Optional M6+/M8 optimization (`cache.put` in `waitUntil`) |

## Open questions (answer during M0)

Riskiest first. Each names the M0 task in [plan.md](plan.md) that answers it.

1. **Do browser decoders deliver partial output mid-meta-block?** The
   one-meta-block streaming layout depends on Chromium's brotli source stream
   (and Firefox/WebKit) handing decoded bytes to the parser before a
   meta-block ends. If not, the fallback layout becomes the primary one. Also
   in scope: whether trailing-whitespace padding after `</html>` is invisible.
   *M0 research: decoder source reading.*
2. **Can rust-brotli's encoder accept an external command list without a deep
   fork?** Where command generation and meta-block writing separate, how
   q11's parse can be extracted for round-trip validation, and the fork
   mechanics (vendored, git dependency, upstreamable patches). *M0 spike: source
   reading plus a throwaway round-trip.*
3. **Does Cloudflare's edge preserve flush boundaries for manually-encoded
   `br` responses?** Timing of bytes reaching the client from a Worker that
   writes small chunks to a `TransformStream` with `encodeBody: "manual"`.
   Includes HTTP/2 and HTTP/3 framing and any edge-side buffering. *M0 spike:
   tiny Worker plus a client-side timing capture.*
4. **How is the versioned model blob stored and delivered to Worker isolates?**
   D-011 settles that runtime shared state in an isolate is immutable per model
   version and upgrade parses fold into the offline trainer (preserving
   cross-isolate determinism and avoiding Durable Objects/Queues in M1–M7).
   Still to pick for the M0 architecture draft: how the blob reaches isolates
   (Static Assets binding as in dictionary-worker, R2, KV, or bundled in the
   Worker), where `waitUntil` sends telemetry/samples, and what "distinct
   sessions" means for the admission gate without storing user identifiers.
   *M0: architecture draft.*
5. **Is the per-request CPU budget enough?** Workers Paid defaults to 30 s
   CPU per request, Free to 10 ms. We need wasm encoder throughput (wasm is
   slower than native) at q1-class and cache-hit paths on the corpus, and
   whether the Free plan is in or out of scope. *Measured in M1 once the wasm
   build exists.*
6. **How do template hints reach the encoder?** For the proxy deployment, an
   origin response header or trailer that the Worker strips is a candidate.
   For third-party crawl data there are no hints at all. *M0 triage; decides
   whether the hint path is measurable before a cooperating origin exists.*
7. **Worker shell: thin JS shell + wasm-bindgen core, or all-Rust
   `workers-rs`?** dictionary-worker used a JS shell around emscripten wasm,
   with a `TransformStream`, `waitUntil` and lazy isolate-global init started
   in parallel with the origin fetch. That shape is proven for this exact
   streaming job. workers-rs keeps everything in Rust but adds a layer between
   us and the stream APIs we need to control flush timing. *M0: decide with
   the spike for question 3.*
8. **Which sites, how many responses per route, and how routes are labeled.**
   Corpus composition (documents vs JSON, two or three templated sites per the
   design), crawl politeness and storage format. *M0 triage, then M1 tooling.*
9. **Design-doc open questions that can ride along:** admission N and its
   privacy variation, cache/model namespace granularity, upgrade-parse cost
   model (current vs neutral tables), prior-art and patent search. These don't
   block M1.
