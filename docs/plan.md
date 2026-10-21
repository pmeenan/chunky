# Plan

This is a living document. Record changes to load-bearing scope in decisions.md;
routine implementation choices do not need entries. Check a box only when the
item is done and verified. Status: `pending` · `in progress` · `done` · `parked`.

## M0: Plan the plan  `in progress`

Goal: settle the feature matrix, architecture and build sequence with the owner.
D-012–D-015 incorporate the design review: exact bytes, approved public learning,
explicit runtime contracts and held-out measurement. No encoder code exists yet.

- [x] Repo scaffolding for the AI-directed workflow.
- [x] Revise the design and milestone scopes to address the review findings.
- [x] Verify that the reference C decoder can return partial literal output
      before a command's copy distance arrives (RE-001); no browser claim implied.
- [ ] Feature triage: resolve the remaining proposed rows in features.md with
      the owner. Parked single-block research does not block the initial encoder.
- [ ] Spike (Q2): inspect rust-brotli command generation/meta-block separation;
      capture and reinject a q11 parse; choose fork mechanics.
- [ ] Spike (Q3 + Q7): manually encoded per-flush `br` from a tiny Worker,
      paced origin markers and client arrival captures over HTTP/2 and HTTP/3.
      Measure delivery, origin-read fragmentation and JS/wasm copy overhead;
      recommend JS shell or workers-rs. Do not require transport chunk boundaries
      to survive. Use the result to name initial resource budgets and headroom.
- [ ] Corpus policy (Q8): choose two or three approved public templated sites,
      crawl politeness, eligible spans, responses per route and corpus format.
      Record capture times, body hashes and route labels; name chronological
      training/tuning/evaluation slices and a template-change slice.
- [ ] Architecture completion (Q4): choose blob delivery/storage, aggregate-only
      telemetry destination, numeric model/stream/pool caps and the commitment
      interface. architecture.md records the required contracts, not measured limits.
- [ ] Toolchain: edition/MSRV, workspace/fork layout, wasm build path, test stack,
      fmt/clippy, local checks or CI, and permissive-license audit.

**Exit:** the owner says the plan is ready to build; Q2–Q4 and toolchain/corpus
choices have answers or named fallbacks. Q1's single-block browser research is
parked; the initial fallback is the accepted per-flush design. M0 remains open
until that owner call; this revision does not claim the spikes are complete.

## Milestone ladder  `pending (scoped; M0 choices remain)`

All reported results name corpus/split/artifact versions and exact commands.
Model/cache/cut rules are frozen before evaluation, with no evaluation-driven
updates. Same-response warm replay is diagnostic, separate from generalization.

### M1: Workspace, corpus and opportunity measurement  `pending`

Cargo workspace, native/wasm reference builds, fmt/clippy/test and license audit.
Crawler, route labels and corpus manifests implement the approved public-data
policy. Record time, body identity and logical-flush provenance separately from
transport fragments. Train/tune on earlier captures and evaluate later ones;
label naturally repeated bodies and obtain or synthesize a template change.

Measure reference q1/q4/q11 size and CPU with equal windows and flush positions,
cross-checking C brotli. Report buffered and streaming cases separately. The
repeatability funnel measures cross-response LZ77/static-dictionary potential,
~256 B CDC, 64/128/256 B and delimiter bias, then prefix/suffix extension with
training-only frozen cut rules. It is not a legal ordinary-br size ceiling.
Run a minimal reference-q1 wasm Worker to measure CPU and resource overhead.

**Exit:** reproducible baseline/funnel reports and wasm measurements. The owner
makes a go/change call and records numerical size-gap recovery, CPU, latency and
memory targets before M3/M4 evaluation. No performance promise is inferred from
chunk overlap alone.

### M2: Command injection and byte-preserving per-flush output  `pending`

Forked encoder accepts external commands with fast and q11 oracle table modes.
Capture and reinject q11 parses; verify byte identity on decode and explain any
size difference from the original parse/table arrangement. Add actual-MLEN
per-flush blocks, bounded block splitting, final literal runs, alignment and EOF.
The independent round-trip verifier, inspector and property tests land here.

**Exit:** every fixture decodes exactly with C brotli and the Rust reference;
cover empty/tiny blocks, unmatched tails, overlapping copies, literal contexts,
short-code ring updates and meta-block boundaries. A fixed logical flush sequence
produces identical output under different feed fragmentation. Initial reference
q1 and external-command paths both pass the same validity/flush checks.

### M3: Offline parse cache on held-out responses  `pending`

Chunker, bounded eligible-public warm set, anchored references, stitcher,
q1-class miss hasher with deterministic candidate/byte-comparison caps, and
training-only frozen cut rules. Train composite windows of eligible chunks;
chunk-anchored targets never cross their target chunk boundary. Resolve short
codes and contexts at emit time. Cold/rejected artifacts use reference q1.

**Exit:** held-out reusable-parse coverage, successfully resolved legal-reference
coverage, size and CPU versus q1/q4/q11 under fast and oracle tables. Report
same-response warm replay separately. Exercise absent/out-of-window targets,
changed templates and adversarial cache states through differential fuzzing;
no invalid bytes or learning-gate bypasses.

### M4: Route models and versioned artifact loading  `pending`

Offline context maps, priors, pruned tables and small-block policy. Implement
`chunky-formats` serialization and validated native/wasm loading for the model
and warm cache, including site/route, chunker and compatibility metadata and
public-data provenance. Pin one artifact per stream. Bad/incompatible artifacts
select the cold path before commitment. Complete single-block tables are deferred
with the parked experiment.

**Exit:** size/CPU ablations for cache alone, model alone and both on frozen
held-out data; cold and stale-model regressions reported by route against the
M1 targets. Matching native/wasm results for identical logical inputs/options;
loader rejects invalid lengths/offsets/versions within a bounded memory budget.
The Worker can use these artifacts without waiting for M7.

### M5: Streaming delivery and continuation  `pending`

Replay per-flush blocks with small-first-flush policy, complete-byte delivery,
backpressure and cancellation. Explicit logical positions drive replay; vary
transport fragmentation independently. Establish valid continuation with
uncompressed blocks in the same stream at healthy completed-block boundaries.

**Exit:** exact round trips and measured total withheld bytes, per-flush CPU and
client delivery latency; include long unmatched tails, empty/short responses,
SSE/ndjson and slow consumers. Inject faults before commitment, within commands,
at block boundaries and EOF. Recoverable cases continue the same stream;
unrecoverable ones abort instead of signaling valid EOF. Record browser/runtime
versions and verify the per-flush path over the Worker edge. Single-block work
is not required to exit M5.

### M6: Worker compressing proxy and aggregate resource bounds  `pending`

Integrate wasm with the chosen shell, immutable M4 artifacts and aggregate-only
telemetry. Correct negotiation and response headers, per-read logical flushing,
bounded completion buffering, and explicit precommit/streaming/completed/aborted
states. Reserve each stream's allowed worst-case allocation before commitment;
account for shared blobs, refresh/growth peaks, JS/wasm transfers and headroom.
Reference-q1 fallback also needs a reservation. Apply backpressure and cancel
origin reads on downstream cancellation; cleanup is mandatory, recording is shed.

**Exit:** real-origin size/CPU/memory/latency against the M1 targets; fault
injection and concurrent long-lived slow-consumer tests remain within budget,
including model refresh. Exhausted capacity rejects compression before commitment
with an acceptable alternate representation or an error. Cover `br;q=0`, HEAD,
no-transform, 304/206, existing encodings, Vary and ETag behavior. Never switch to
identity bytes or a fresh compressor after committing a br response.

### M7: Fleet rollout and model freshness  `pending`

Use the M4 loader to validate versioned publication, bounded refresh/rollback,
fresh isolates, stale models and template changes. No live private-traffic learning.

**Exit:** native/wasm/cross-isolate determinism under identical bytes, logical
flush positions and options; size/CPU for fresh versus stale artifacts; pinned
in-flight versions and bounded update overlap. Different transport-derived flush
schedules may produce different bytes and are labeled accordingly.

### M8: Standard CDT static variants  `pending`

Sidecars, chunk dedup, localized search and offline parse recovery from build
`.br` with explicit distances/static-word references. Add Shared Brotli emission
and RFC 9842 dcb framing, dictionary identity, negotiation, Vary and server security
rules. Use capped `max_distance` geometry, not uncapped response position.

**Exit:** independent Shared Brotli decode equals input; tests exercise references
before/after window saturation, dictionary start/end, short codes and built-in
static words. Plain-br clients never receive external-dictionary references.
Measure generation time and size against full q11-with-dictionary, including
framing bytes and dictionary memory. Dictionary plus response need not fit the
rolling window.

### M9: Generated public site dictionary  `pending`

Build from eligible approved public chunks, audit before publication and serve
with `Use-As-Dictionary`. Store logical dictionary-relative cache references.

**Exit:** provenance/eligibility checks cover every published chunk; negotiated
dcb round trips and plain-br fallback pass. Measure document size with/without
CDT on held-out responses, including dictionary acquisition cost separately.

## Exact-length single-meta-block experiment  `parked (after M5)`

Only exact, trusted uncompressed lengths; never predicted MLEN or decoded padding.
Complete tables encode all reachable symbols/distances. Investigate partial
literals/copies, pending command fields and alignment without asserting a 2–4 B
delay bound. Browser and Worker marker delivery, EOF mismatch, unmatched tails,
byte identity and resource usage must meet the established per-flush targets.
Promote only if held-out size/CPU benefits justify the complexity; no initial
milestone depends on success.
