# Feature matrix

This is the scope ledger for the M0 planning conversations. It has three tiers:

- **Confirmed**: stated project scope, from [design.md](design.md) (adopted by
  D-001) or the owner's kickoff choices. Milestone assignment happens in
  [plan.md](plan.md) as the plan firms up.
- **Proposed**: candidate additions awaiting a yes/no from the project owner.
- **Open questions**: things that shape architecture and need an answer
  during M0.

Status legend: `confirmed` · `proposed` · `parked (D-NNN)` · `rejected (D-NNN)`

Confirmed rows describe *what* is in scope. The design doc holds the *how*, and
the section to read is named in Notes.

## Encoder core (library, wasm-compatible)

| Feature | Status | Notes |
| --- | --- | --- |
| Command-list injection: forked rust-brotli entropy coder and meta-block writer accept an externally produced command list (supporting both fast single-table coding and q11 oracle table coding) | confirmed | design: Format choice, Milestone 2. The primitive everything else hangs off. Validate by round-tripping q11's own parse; oracle table mode lets M3 isolate parse-cache gains before M4 lands |
| Content-defined multi-resolution chunker (~256 B fine, ~4 KB coarse levels from one rolling hash, with optional delimiter/tag bias) | confirmed | design: Parse cache, Chunker resolution. Parameters are a versioned, deployment-wide constant |
| Parse cache: chunk fingerprint → commands with strictly chunk-contained `chunk-anchored`, `dictionary-anchored` and `predicted-distance` logical references; prefix index; stitcher resolving distance short codes (`0..15`) and literal contexts (`p1`, `p2`) at emit time | confirmed | design: Parse cache; D-010 |
| Slot handling: prefix extension to divergence, learned cut rules | confirmed | design: Parse cache → Slots |
| q1-class fast hasher for misses, with predicted-distance hints and deterministic candidate/byte-comparison caps | confirmed | design: Parse cache → Misses; D-014. Wall-clock checks may cancel a wrapper but cannot change valid output within the determinism contract |
| Route model: pre-trained context map, block-type policy, priors, table ladder, size distributions | confirmed | design: Entropy and modeling state |
| Prior-smoothed pruned Huffman tables per available meta-block, small-block size policy | confirmed | design: Entropy and modeling state; complete unpruned tables belong to the parked exact-length experiment |
| Byte-preserving single-meta-block encoding with an exact trusted uncompressed length | parked (D-012) | After M5: complete tables and browser/edge delivery experiments; no initial milestone depends on it; no assumed 2–4 B delay bound |
| Estimated MLEN with decoded whitespace shortfall padding | rejected (D-012) | Changes input bytes and conflicts with the lossless contract |
| Per-flush meta-blocks with actual MLEN, small-first-flush policy and quantized table ladder | confirmed | Initial streaming layout, including SSE/ndjson; complete-byte delivery without waiting for later input (D-012) |
| Two-axis response-mode selection from headers (size known × streaming matters), checking unencoded `Content-Length` | confirmed | design: Response modes; D-011 |
| Runtime content scanning: base64/data URIs, embedded sublanguages, high-entropy tokens get dedicated block types / tables | confirmed | design: Content-aware optimizations |
| Template hints: slot boundaries, slot types, secret slots | confirmed | design: Content-aware optimizations. How hints reach the encoder is open question 6 |
| Approved public source/span eligibility gate and literal-only secret slots | confirmed | D-013 supersedes D-009. All learned artifacts use the gate; frequency and token detection are heuristics, not privacy proofs |
| Cold-cache / rejected-model path selects reference q1 with matched window/flush policy | confirmed | D-014. First-class test configuration; warm/stale quality is measured rather than assumed monotonic |
| Deterministic output for identical bytes, logical flush positions, versions, dictionary and options | confirmed | D-014; different feed fragmentation alone must not change output; Worker transport-read flush schedules may vary |
| Demand-grown buffers and zero-copy dictionary slices within an isolate-wide reserved pool | confirmed | D-014 extends D-011: reserve worst-case stream capacity before commitment; account for shared state, growth/refresh peaks, JS/wasm copies and headroom |

## Learning and models

| Feature | Status | Notes |
| --- | --- | --- |
| Live finalization: bounded aggregate performance recording and mandatory resource cleanup | confirmed | D-013/D-014; no bodies, fingerprints, content histograms or whole-response training samples from live traffic |
| Upgrade path: q11-quality parses for admitted chunks against composite admitted-chunk route windows (commands clamped/split at chunk boundaries), computed off-request | confirmed | design: Learning architecture; D-010/D-013; offline only in the initial deployment |
| Offline public-data trainer producing a versioned model + warm cache + frozen cut rules | confirmed | Source/span gate applies before all derivation, including samples and statistics; M4 packaging/loading, M7 fleet validation |
| Versioned on-disk formats with bounded structural/compatibility validation and per-stream artifact pinning | confirmed | M4 implements model/warm-cache native/wasm serialization and loading before Worker integration; mismatches select q1 before commitment (D-014/D-015) |

## Dictionaries and CDT (secondary target)

| Feature | Status | Notes |
| --- | --- | --- |
| Dictionary sidecar (chunk index, prefix index, stats, geometry, cut rules) | confirmed | design: Dictionaries and CDT → sidecar. "mmap" becomes "load a blob" in the Worker |
| CDT static variants: chunk dedup, localized search, offline build-parse recovery, Shared Brotli encoding and dcb framing/negotiation | confirmed | D-012: capped distance base, external dictionary outside rolling window, 36-byte header and identity, correct Vary and RFC 9842 server rules; independent Shared Brotli verification |
| Generated site dictionary from eligible approved public chunks, served via Use-As-Dictionary | confirmed | D-013 applies before publication; document references require negotiated dcb, never hidden dictionary references in plain br |

## CLI tools (build and test)

The owner's kickoff ask: "a collection of CLI tools for building and testing
it." Load-bearing measurement and verification tools are confirmed; later-stage
or optional tools remain proposed or scoped to later milestones.

| Feature | Status | Notes |
| --- | --- | --- |
| Corpus crawler/collector: logged-out polite crawl, stores bodies + headers + route labels in the local data dir | confirmed | D-007; design: Milestone 1 |
| Route-labeling tool: cluster crawled URLs into routes (path templates) | confirmed | Third-party crawls carry no route ids (D-007); every per-route metric in M1+ depends on this |
| Baseline tool: reference q1/q4/q11 size and CPU per response/route with matched windows and logical flushes | confirmed | D-015; cross-check C google/brotli, report native/wasm separately |
| Chunk-overlap analyzer: cross-response/static-dict reuse potential, CDC scales/bias, prefix extension and training-only frozen cut rules | confirmed | D-015; held-out later captures, duplicate-body reporting, reusable parse versus legal-reference coverage; overlap is not an ordinary-br size ceiling |
| Encoder CLI: compress a file or corpus with a given cache/model, report size, CPU and mechanism breakdown | confirmed | Needed for milestones 3 to 5 measurements |
| Model/cache trainer CLI (the offline fleet model) | confirmed | Same as the Learning row above, surfaced as a tool |
| Round-trip verifier: independent decoder plus exact byte comparison | confirmed | C/Rust RFC 7932 decoding for br from M2; independent Shared Brotli decoding with the negotiated dictionary for dcb in M8 |
| Stream inspector: meta-blocks, commands, description costs and logical flush/alignment points | confirmed | M2 onward; measures per-flush overhead and withheld bytes; no padding policy |
| Flush replay harness: bytes plus logical flush positions and timing, with transport fragmentation varied independently | confirmed | M5 delivery and determinism tests; public-crawl reads alone do not reveal origin application writes |
| Benchmark report generator: comparable q1/q4/q11 tables with corpus/split/artifact versions and exact commands | confirmed | D-015; held-out temporal and template-change slices; exact-response warm replay labeled separately |
| Differential fuzzing / property tests: inputs/cache states/fragmentation and stream failures | confirmed | M2 validity, M3 stitched refs, M5 continuation/abort and flush delivery; dcb geometry in M8 |
| Sidecar builder CLI for CDT dictionaries | confirmed | Packaging of the confirmed sidecar feature for M8 |

## Cloudflare Worker (compressing proxy)

| Feature | Status | Notes |
| --- | --- | --- |
| Worker proxy: fetch from origin, compress eligible responses with the wasm core, return with `encodeBody: "manual"`, strip `Content-Length` and weaken strong `ETag`s | confirmed | D-004, D-011. Prior art: the owner's [dictionary-worker](https://github.com/pmeenan/dictionary-worker) (Apache-2.0); see [architecture.md](architecture.md) |
| Content negotiation: accept br only with nonzero quality; preserve Vary and bypass ineligible responses | confirmed | Verify original client encodings in Worker; cover HEAD, no-transform, 304/206 and existing encodings; dcb negotiation added in M8 |
| Streaming delivery: flush available origin-read bytes using actual-length blocks with backpressure/cancellation | confirmed | D-012/D-014; reads are transport fragments, not stable application flush boundaries; paced-marker edge measurements in Q3 |
| Same-tick origin-read coalescing | parked (D-014) | Needs measured scheduling/delivery effects; initial Worker flushes nonempty reads |
| Mode selection from request/response headers (Sec-Fetch-Dest, Priority, unencoded Content-Length, Content-Type, Cache-Control) | confirmed | design: Response modes; D-011 |
| Load immutable model/warm cache, pin per stream and bound refresh overlap | confirmed | Loader lands M4, Worker integration M6, fleet freshness/rollout M7 (D-015) |
| State-dependent fallback: precommit alternate representation, healthy same-stream continuation, otherwise abort | confirmed | D-014; no fresh compressor or identity suffix after commitment; reserve even q1 fallback memory |
| Strip origin-supplied template-hint headers before the response reaches the client | confirmed | Lets a cooperating origin pass hints via response headers at zero client wire cost (open question 6) |
| Shadow / comparison mode: also compute reference q1 size off-path and log the delta | proposed | Optional M6+ diagnostic over ephemeral bytes; no training export or retained bodies, and reserve its CPU/memory budget (D-013/D-014) |
| Debug response header (opt-in) reporting cache hit rate, mode, bytes saved | proposed | Optional M6+ diagnostic; off by default because it exposes learned state; D-013 |
| Metrics via Workers Analytics Engine or logs: bounded aggregate CPU/byte/error counters | proposed | Optional M6+ destination; excludes URLs, content identifiers and training data (D-013) |
| Kill switch / per-route enable list | proposed | Optional M6+ operational control |
| Cache API storage of compressed output for cacheable (`public`) responses | proposed | Optional M6+/M8 optimization (`cache.put` in `waitUntil`) |

## Open questions (answer during M0)

Q numbers are stable so plan/architecture references remain meaningful. Named
fallbacks unblock the initial encoder; only the parked experiment needs Q1.

1. **Do browsers deliver partial decoded output mid-meta-block?** The C decoder
   exposes partial literals (RE-001); Chromium/Firefox/WebKit parser delivery
   remains unverified. Park this research with the exact-length single-block
   experiment after M5. Initial per-flush blocks need their own delivery checks;
   decoded whitespace padding is rejected, not a browser-compatibility question.
2. **Can rust-brotli accept external commands without a deep fork?** Inspect
   generation/writer separation, capture/reinject q11 and pick fork packaging.
   *M0 source/round-trip spike.*
3. **Does the Worker edge deliver manually encoded per-flush br promptly?**
   Use paced origin markers and client arrival times over HTTP/2 and HTTP/3.
   Record arbitrary read fragmentation and JS/wasm transfer overhead; do not
   require transport chunk boundaries to survive. *M0 Worker spike.*
4. **How are immutable blobs delivered within a bounded memory budget?**
   Choose bundled blobs, Static Assets, R2 or KV; reserve loading/refresh peaks,
   pin versions per response and name numeric stream/pool caps and headroom.
   Select an aggregate-only telemetry destination. M4 implements the loader;
   live learning and distinct-session identities are unnecessary (D-013).
   *M0 architecture choices and spike measurements.*
5. **Is wasm CPU sufficient?** Measure reference and later cache-hit paths on
   the corpus; select a Workers plan and check its current caps. Include
   per-flush CPU, cancellation and JS/wasm overhead. Hard runtime termination
   is not a recoverable fallback promise. *M1 baseline, M5/M6 validation.*
6. **How do template hints reach the encoder?** An origin header or other
   interface must be stripped before any outgoing response, including bypass.
   Third-party crawls have no hints, so evaluate with synthetic/cooperating
   origins. Literal-only secret hints do not prove table/context isolation.
   *M0 triage; D-013.*
7. **Worker shell: thin JS plus wasm or workers-rs?** Use Q3 measurements to
   choose the stream/cancellation and buffer-transfer interface. `waitUntil`
   manages lifecycle, not a background CPU thread. *M0 Worker spike.*
8. **Which approved public sites and capture splits?** Pick sites, politeness,
   response counts, route labels, provenance and source/span exclusions. Name
   chronological training/tuning/evaluation slices, duplicate-body reporting
   and a template-change slice. *M0 policy, M1 tooling; D-013/D-015.*
9. **Questions that can ride along:** usefulness-frequency thresholds on
   eligible public captures, route/type namespace within tenant/site partition,
   upgrade-parse cost model and prior-art/patent search. Private learning is
   excluded; it cannot be enabled by merely choosing an admission N.
