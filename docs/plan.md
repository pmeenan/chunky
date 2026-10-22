# Plan

This is a living document. Record changes to load-bearing scope in decisions.md;
routine implementation choices do not need entries. Check a box only when the
item is done and verified. Status: `pending` · `in progress` · `done` · `parked`.

## M0: Plan the plan  `in progress`

Goal: settle the feature matrix, architecture and build sequence with the owner.
D-012–D-015 incorporate the design review: exact bytes, approved public learning,
explicit runtime contracts and held-out measurement. D-016–D-017 add the
quality-curve baseline, q2-class cold path, emit-time verification and the
retained-history cap. D-018 (accepted) sets the initial Google Search corpus.
The Q2 spike answered command injection and fork mechanics (D-020). D-019
relaxes private/Set-Cookie ineligibility for offline prototype artifacts. No encoder code exists in the repo yet.

- [x] Repo scaffolding for the AI-directed workflow.
- [x] Revise the design and milestone scopes to address the review findings.
- [x] Verify that the reference C decoder can return partial literal output
      before a command's copy distance arrives (RE-001); no browser claim implied.
- [x] Probe reference encoder match reach across input calls and flushes:
      q0/q1 never match across calls (RE-002, D-016).
- [x] Owner accepts or revises D-018, the D-013 eligibility exception for
      manually captured Google Search netlogs. Accepted 2026-10-21. The owner
      also relaxed the private/Set-Cookie rule for prototype artifacts (D-019).
- [x] Feature triage: the four optional Worker diagnostics/controls are
      confirmed. Cache API storage stays proposed until the Q3 spike tests
      Workers Cache (see that row). Parked single-block research does not block
      the initial encoder.
- [x] Spike (Q2): command injection needs no deep fork. A ~150-line entry
      point feeds external commands to the existing meta-block writer, and
      captured q11 parses round-trip through the C decoder in oracle, fast and
      per-flush modes. Default builds differ native vs wasm at q11 (libm
      `log2`/`powf`); `portable-float` makes them identical (RE-003). Fork
      mechanics: D-020. The spike code lived in a session scratchpad;
      M2 rebuilds it in the workspace.
- [x] Spike (Q3 + Q7): done 2026-10-21 (D-021, RE-005–RE-007).
      - **Delivery:** per-read flushed br arrives promptly over HTTP/2 and
        HTTP/3 once `no-transform` is sent.
      - **Negotiation:** must use `clientAcceptEncoding`, which keeps
        dcb/dcz.
      - **Cache:** Workers Cache is byte-exact for br but needs a normalizing
        outer entrypoint for dcb.
      - **Cost:** JS/wasm copies are 2.5–6.7% of encode time; peak wasm memory
        is 14–20 MiB per encoder at lgwin 22.
      - **Fragmentation:** origin reads fragment unlike origin writes (one
        2,032 B write arrived as reads of 27 B and 2,005 B).
      - **Real origin** (owner's nginx behind the route):
        `Accept-Encoding: identity` is not honored, because the origin always
        receives gzip. The Worker sees decoded bytes with no `Content-Encoding`
        or `Content-Length`, in about 305 reads per 534 KB page (median 70 B,
        max 4 KiB).
      - **Flush policy:** flushing every read costs +12.5% (q2) and +8.8%
        (q5). Coalescing already-available reads matched the unflushed size
        and kept paced-marker latency.
      - **Budgets:** carried into the architecture item below (wasm memory
        per encoder, CPU per MB).

      Original scope: manually encoded per-flush `br` from a tiny Worker,
      paced origin markers and client arrival captures over HTTP/2 and HTTP/3.
      Measure delivery, origin-read fragmentation and JS/wasm copy overhead;
      recommend JS shell or workers-rs. Do not require transport chunk boundaries
      to survive. Record the origin-read size distribution, since it sets
      per-flush header overhead. Test whether an `Accept-Encoding: identity`
      origin request is honored, yielding a trusted Content-Length and skipping
      origin compression plus edge decompression. Check whether
      `request.cf.clientAcceptEncoding` preserves `dcb`/`dcz`. Test whether
      Workers Cache stores `encodeBody: "manual"` br bodies byte-exact and
      serves the right variant per restored `Accept-Encoding` (the Cache API
      row in features.md). Use the result to name initial resource budgets and
      headroom. Docs checked 2026-10-21 report 128 MB per isolate (JS + wasm),
      Paid CPU up to 5 min per request (`limits.cpu_ms`), and 25 MiB per Static
      Asset or KV value (R2 for larger blobs). Recheck before relying on them.
- [x] Spike (Q8, extraction): [tools/netlog/](../tools/netlog/) holds
      `extract.py` and the `flushscan` byte-at-a-time boundary scanner. It
      covers the first capture: 2026-10-21, Chrome 156 on Windows desktop, HTTP/3.
      - **Bytes:** `URL_REQUEST_JOB_BYTES_READ` carries the served bytes,
        including the 36 B dcb header; `..._FILTERED_BYTES_READ` carries the
        decoded bytes. Both have timestamps.
      - **Dictionary:** `/shared_dict/srp/<sha256>.dict` with
        `match="/search*"`, 261,354 B. Its SHA-256 equals the
        `Available-Dictionary` value.
      - **Bodies:** all 23 `/search` documents (1 br, before the dictionary
        loaded, and 22 dcb) decode with C libbrotlidec to exactly the netlog's
        decoded bytes. One non-Google document was skipped.
      - **Flushes:** 440 byte-aligned meta-block ends recovered, every one
        validated by C (prefix + `0x03` decodes to the prefix). At all 602
        transport read boundaries, every decodable one was found; the other 267
        were mid-block.
      - **Redaction:** `cookie`, `set-cookie`, `x-client-data` and auth header
        values are redacted.
      - **Consent/CAPTCHA:** none flagged; the heuristic is untested on a
        positive case.
      - **Flush pattern:** Google flushes the 15 B doctype at about 40 ms, then
        roughly 10 KB median blocks. A final block can exceed 1 MB.
      - **Not done:** excluded-span detection (nonces, per-response ids) is
        left to corpus policy and M1.
- [x] Quick opportunity probe: **go.** Size only; system C brotli 1.1.0,
      lgwin 22; 23 responses, 24.2 MB decoded; throwaway scripts. One capture
      session with no held-out split, so these are indicative numbers, not an
      evaluation.

      | Config (total bytes; ratio vs buffered q11) | Buffered | Google's flush schedule |
      | --- | --- | --- |
      | q1 | 6,158,285 (1.335) | 6,514,692 (1.412) |
      | q2 | 5,559,915 (1.205) | 5,565,260 (1.206) |
      | q5 | 5,013,758 (1.087) | 5,041,818 (1.093) |
      | q9 | 4,886,121 (1.059) | 4,886,818 (1.059) |
      | q11 | 4,613,409 (1.000) | 4,657,386 (1.010) |
      | q11, no literal context modeling | 4,643,011 (1.006) | 4,676,832 (1.014) |
      | q11, lgwin 18 | 4,971,157 (1.078) | — |
      | q5 with Google's dictionary | 3,808,663 (0.826) | 3,832,143 (0.831) |
      | q11 with Google's dictionary | 3,558,668 (0.771) | 3,584,717 (0.777) |

      - **Cold-path gap:** the target cold path (q2, streamed on Google's
        schedule) is 20.6% bigger than q11 streamed the same way. Google's
        flushes cost q2+ almost nothing; q1 pays 5.5% (RE-002).
      - **Where the gap is:** q9→q11 parsing is 5.9%. Literal context modeling
        is only 0.6%. Capping history at 256 KB (lgwin 18) costs 7.8%, so the
        retained-history cap matters on these 0.36–2.3 MB pages.
      - **Dictionary:** Google's dictionary saves 23% at q11. At q2 it saves
        nothing, because C 1.1.0 only uses an attached dictionary from q5 up
        (RE-004). chunky's own paths are not bound to that cutoff.
      - **What Google served:** 3,653,239 B on the 22 dcb pages. That lies
        between our dictionary q5 and q9 on the same flush schedule, and is
        6.0% above dictionary q11. The one br page was 197,044 B against our
        q11's 184,323 B.
      - **Chunk reuse:** naive Gear CDC, chronological, 22 later pages with all
        queries different. At 256 B average chunks, 21.9% of bytes are in a
        chunk seen in an earlier response (64 B: 33.0%, 1 KB: 13.7%); 5.0% are
        in a dictionary chunk. This is reusable-parse potential, not a size
        ceiling (D-015).
- [x] Corpus policy (Q8): D-022 (accepted). Sessions are manual and
      extension-assisted (`tools/capture/`, published at
      `chunky.meenan.dev/capture/`): one Pixel 10-emulated incognito tab steps
      through the Google panel, fresh queries, then Etsy categories and
      listings. Second sources: Wikimedia and Shopify storefronts from Common
      Crawl (owner's choice). Sample measurements wait until Common Crawl's CDN
      stops throttling this host (RE-008). Etsy stays in each capture session
      unless it blocks the owner's browser too. Span survey of the first
      session: named per-response carriers are about 1.7% of decoded bytes;
      the `DV` cookie value is echoed in every page; `sca_esv` repeats within
      the session. `manifest.jsonl` from the extraction spike is the draft
      manifest format. Finalize the D-018 query mix, locale, session
      cadence, excluded-span rules, responses per route and manifest format. Name
      chronological training/tuning/evaluation slices and a template-change
      slice. Name the second site (or a Common Crawl fallback) needed before
      generalization claims.
- [x] Architecture completion (Q4): D-024 (accepted): bundled
      artifacts (64 MiB bundle limit), a 96 MiB application cap with fixed
      stream slots, the edge-compression fallback when no slot is free, slot
      exports as the commitment interface, and Analytics Engine telemetry.
      Spike inputs: 14–20 MiB wasm memory per
      lgwin-22 encoder (8.4 MiB at lgwin 18); about 9 ms (q2) and 19 ms (q5)
      of V8 encode per MB, on the Workers Paid plan; 25 MiB per Static Asset or
      KV value. Choose blob
      delivery/storage, aggregate-only
      telemetry destination, numeric retained-history/model/stream/pool caps and
      the commitment interface. architecture.md records the required contracts,
      not measured limits.
- [x] Toolchain: D-023 (accepted): Rust 1.99.1 pin, edition 2024,
      vendored rust-brotli and C brotli v1.2.1 (clang native, wasi-sdk wasm32),
      `scripts/check.sh` locally and in GitHub Actions, cargo-deny, and a
      self-locking `chunky bench`. clang 18.1.3 and lld installed. Scope:
      edition/MSRV, workspace/fork layout, wasm build path, test stack,
      fmt/clippy, local checks or CI, and permissive-license audit. Pin the C
      `google/brotli` baseline (commit, clang flags, native and wasm32 builds)
      and choose how the harness calls it: a small C driver or an FFI crate
      with checked license metadata. Decide whether
      the netlog extractor starts as a script or a `chunky-cli` subcommand, and
      how CPU benchmarks run on a shared measurement host (workflow.md).

**Exit:** the owner says the plan is ready to build; Q2–Q4 and toolchain/corpus
choices have answers or named fallbacks, and the opportunity probe has not
returned a no-go. Q1's single-block browser research is parked; the initial
fallback is the accepted per-flush design. M0 remains open until that owner
call; this revision does not claim the spikes are complete.

## Milestone ladder  `pending (scoped; M0 choices remain)`

All reported results name corpus/split/artifact versions and exact commands.
Model/cache/cut rules are frozen before evaluation, with no evaluation-driven
updates. Same-response warm replay is diagnostic, separate from generalization.
Success means a Pareto improvement over the measured reference quality curve
(D-016); gap recovery over q1 is a diagnostic.

### M1: Workspace, corpus and opportunity measurement  `pending`

Cargo workspace, native/wasm reference builds, fmt/clippy/test and license audit.
The netlog extractor, served-stream flush recovery, route labels and corpus
manifests implement D-018 and the approved public-data policy. Measurements
run offline on the extracted files.
Record capture time, body identity, the served encoding and dictionary hash, and
logical flush offsets separately from transport fragments. Train/tune on earlier
captures and evaluate later ones; label naturally repeated bodies and obtain or
synthesize a template change.

Measure the reference curve (at least q1–q6, q9 and q11) for size and CPU on
identical bytes and flush schedules with both pinned C `google/brotli` (the CPU
baseline, D-016) and rust-brotli at the fork base, driven through the same
call/flush sequence. Record C version, compiler and flags. Report each
quality's effective match reach (RE-002), buffered and streaming cases
separately, and synthetic per-read schedules alongside the recovered ones. Report
what Google served, and the reference curve with Google's dictionary attached
(Shared Brotli, dcb framing bytes counted). Decompose the q1→q11 gap with
reference-encoder settings: context modeling on/off, q9 vs q11 parsing, lgwin,
static-dictionary share. The repeatability funnel measures cross-response
LZ77/static-dictionary potential, ~256 B CDC, 64/128/256 B and delimiter bias,
then prefix/suffix extension with training-only frozen cut rules. It is not a
legal ordinary-br size ceiling. Run a minimal reference-encoder wasm Worker to
measure CPU and resource overhead. Measure wasm CPU in V8 (workerd), not
wasmtime, with the pinned C source also compiled to wasm32 as the Worker CPU
baseline. Check that rust-brotli produces identical bytes natively and in wasm.

**Exit:** reproducible baseline/funnel/decomposition reports and wasm
measurements. The owner makes a go/change call and records Pareto targets
(size, CPU, latency, memory) against the reference curve before M3/M4
evaluation. Using the decomposition, the owner also decides (a) whether the
route model (M4) comes before the parse cache (M3), and (b) whether dcb with an
existing document dictionary (Google's) moves ahead of M5. No performance
promise is inferred from chunk overlap alone.

### M2: Command injection and byte-preserving per-flush output  `pending`

Forked encoder accepts external commands with fast and q11 oracle table modes.
Capture and reinject q11 parses; verify byte identity on decode and explain any
size difference from the original parse/table arrangement. Add actual-MLEN
per-flush blocks, bounded block splitting, final literal runs, alignment and EOF.
Every emitted copy and dictionary reference is byte-verified against retained
history or the dictionary slice before emission (D-017). Retained history is a
configured cap, separate from the declared WBITS. The independent round-trip
verifier, inspector and property tests land here, including a native-vs-wasm
byte-identity test that runs with the regular checks.

**Exit:** every fixture decodes exactly with C brotli and the Rust reference;
cover empty/tiny blocks, unmatched tails, overlapping copies, literal contexts,
short-code ring updates and meta-block boundaries. A fixed logical flush sequence
produces identical output under different feed fragmentation, natively and in
wasm. Deliberately wrong injected commands are caught by emit-time verification,
never emitted. The q2-class cold path and external-command paths both pass the
same validity/flush checks.

### M3: Offline parse cache on held-out responses  `pending`

Chunker, bounded eligible-public warm set, anchored references, stitcher,
q1-class miss hasher with deterministic candidate/byte-comparison caps, and
training-only frozen cut rules. Training parses run over real eligible
responses; chunks overlapping excluded spans are not admitted, and references
into them are dropped (D-017). Chunk-anchored targets never cross their target
chunk boundary. Resolve short codes and contexts at emit time. Cold/rejected
artifacts use the q2-class reference path (D-016).

**Exit:** held-out reusable-parse coverage, successfully resolved legal-reference
coverage, size and CPU against the reference curve under fast and oracle tables. Report
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
backpressure and cancellation. Explicit logical positions drive replay, using
flush schedules recovered from served streams; vary transport fragmentation
independently. Measure flush policy as a knob: flush every read, coalesce reads
that are already available, or require a minimum byte count unless the source
is idle. Report table-header overhead against delivery latency for each. Establish valid continuation with
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
The q2-class fallback also needs a reservation. Apply backpressure and cancel
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
