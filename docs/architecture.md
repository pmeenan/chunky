# Architecture

> **Status: contract draft.** Toolchain, fork mechanics, Worker shell, blob
> delivery and numeric resource caps remain M0 choices. This document specifies
> the required interfaces and behavior; it does not claim they are implemented.
> The encoder mechanisms are in [design.md](design.md).

## Fixed points

- Ordinary responses use RFC 7932 `br`; optional negotiated CDT uses RFC
  9841/9842 `dcb`. Independent decoders must recover the exact input (D-012).
- Rust core, CLIs and wasm share one workspace, using a rust-brotli fork
  (D-003). Cloudflare Workers is the server deployment (D-004).
- Streaming initially completes actual-length meta-blocks at logical flushes.
  Exact-length single-block work is parked after M5; no decoded padding (D-012).
- Runtime models/caches are immutable and pinned per stream. All training is
  offline from approved public sources and eligible spans (D-013/D-017).
- Determinism includes logical flush positions and encoder options, and uses
  deterministic work limits. Network scheduling is not a deterministic input
  boundary (D-014).
- Memory admission and fallback are isolate/stream-state contracts, not
  consequences of demand-grown buffers (D-014). Reservations use the
  retained-history cap, not `1 << lgwin` (D-017).
- Every emitted copy and dictionary reference is byte-verified at emit time.
  Artifacts are hints and cannot cause wrong output bytes (D-017).
- The cold, rejected-model and fallback encoder is reference q2-class, because
  q0/q1 never match across input calls (D-016, RE-002).

## Workspace

Names and fork packaging remain M0 choices.

| Crate / dir | Role | Targets |
| --- | --- | --- |
| `brotli` fork | External commands, fast/q11 oracle tables, per-flush writer, emit-time verification and the reference quality curve | native, wasm32 |
| `chunky-core` | Chunking, immutable cache lookup, reference resolution, bounded miss search, tables and streaming encoder | native, wasm32 |
| `chunky-formats` | Validated versioned model/warm-cache blobs in M4; sidecars in M8 | native, wasm32 |
| `chunky-cli` | netlog extraction (or an M0 script first), served-stream flush recovery, label, baseline, overlap, encode, train, verify, inspect, replay, bench, sidecar | native |
| C baseline harness | Pinned C `google/brotli` built with documented clang flags, natively and to wasm32, driven with the same call/flush sequence as the other encoders; benchmark-only (D-016) | native, wasm32 |
| `worker/` | Negotiation, origin fetch, artifact loading, memory admission, stream pump and cancellation | wasm32 plus JS if selected |

The core has no filesystem, threads or clocks. Wrappers supply buffers, explicit
logical flush events and immutable artifact handles. Work budgets count search
candidates/bytes compared; clock-based cancellation belongs to the wrapper and
cannot select different valid encoded bytes within the determinism contract.
Fixed iteration and tie-breaking rules apply across native and wasm builds.

## Core stream interface and determinism

Conceptually the interface accepts `feed(bytes)`, `flush()` and `finish()`.
Feeds can be split arbitrarily; logical flush positions are part of the input.
Empty flushes and repeated flushes at the same offset are no-ops. Options include
route/model/cache/chunker versions, dictionary identity, lgwin, mode and work
limits. Identical bytes, flush positions and options produce identical output.

Each actual-length block is fully parsed/countable before its header is emitted.
Blocks close at the configured cumulative byte cap independently of feed boundaries. A flush
ends the pending block, permits a final literal run and aligns delivery as needed.
EOF closes the same brotli stream. No API discards an open block and silently
starts a new independent stream. The parked exact-length path must distinguish
EOF success from length mismatch and handle pending command fields explicitly.

The Worker derives logical flushes from origin reads through a flush policy:
flush every nonempty read, coalesce reads that are already available, or wait
for a minimum byte count unless the source is idle. M5 measures each policy's
header overhead against delivery latency; the initial policy flushes every read.
Replaying the same body with a different read schedule or policy can change
compression. Core fragmentation tests hold logical flush positions fixed.
End-to-end delivery tests measure paced markers, not preservation of origin
writes as read chunks.

## Isolate-wide memory budget

Cloudflare's 128 MB limit includes runtime/wasm allocations; wasm linear memory
retains its high-water allocation. Demand-grown windows reduce typical allocation
but cannot prove safety under concurrency. Before M6, the Worker spike sets an
application allocation cap strictly below the limit, with measured runtime
headroom. Account for:

- Shared immutable wasm/model/cache/dictionary/sidecar storage, including any
  backing JS copies and old/new versions during refresh.
- Reserved per-stream maxima: retained-history cap (not the declared
  `1 << lgwin`), hasher/index, chunk and command storage, table scratch,
  input/lookahead, completion buffer, queued output and JS/wasm transfer
  copies. Include q2-class fallback allocations.
- Allocator fragmentation and transient growth peaks; reusing a bounded pool
  must not cause unaccounted new wasm memory growth.

Compression admission reserves the allowed worst-case stream allocation before
headers or bytes commit. Allocate lazily within that reservation. Reserve shared
model-load/refresh peaks before loading; retain the old version or reject an
update if capacity is unavailable. Pin one artifact until its streams finish.
Dictionary references read the shared slice directly, using Shared Brotli's
separate address region; dictionary bytes are not copied into each stream window.

If a reservation fails, select an acceptable pass-through/identity representation
or a separately budgeted q2-class path before commitment; otherwise return an error.
Do not reduce a committed lgwin or wait indefinitely for compression capacity.
Bound the pump's input/output queues, await downstream capacity before reading
more origin input, and release streams/reservations on completion, failure or
cancellation. Memory tests include long responses, slow consumers, many active
streams and simultaneous model refresh. Record actual peaks and application
accounting, not just initial buffer sizes.

## Offline data and artifact flow

```
approved capture corpus (manually captured netlogs, local, D-018)
    -> netlog extractor: cookies stripped, encoded/decoded bytes, dictionaries, flush offsets
    -> source/span eligibility filter
    -> chronological training / tuning / held-out evaluation manifests
    -> train and freeze model + warm cache + cut rules
    -> M4 versioned blob -> validated native/wasm loader -> Worker

held-out evaluation -> baseline / overlap / encode / replay / bench reports
live Worker requests -> bounded aggregate performance counters only
```

Raw captures stay local under D-007. Eligibility applies before deriving all
training histograms, samples and chunk sequences (D-013); no whole-response
sampling exception exists. Manifests record provenance, route, capture time,
body identity and logical-flush provenance. Frozen evaluation never updates the
artifacts. Separate same-response warm replay and naturally repeated bodies from
new-response generalization; include stale/template-change tests (D-015).

Blob compatibility covers site/route, chunker and encoder/format versions.
Validate lengths, offsets and allocation bounds before exposure to the core.
A mismatch selects the q2-class reference path before commitment. Serialization/loading lands
in M4; M7 validates rollout and freshness, not the initial loader.

## Worker request path

Patterned on [dictionary-worker](https://github.com/pmeenan/dictionary-worker),
with explicit admission, commitment and cancellation behavior:

1. Start bounded lazy wasm/model init alongside origin fetch; resolve artifact
   compatibility, mode and memory admission before committing the outgoing response.
2. Negotiate the client's original encodings (including q-values; the Worker may
   receive normalized Accept-Encoding). Check `request.cf.clientAcceptEncoding`
   behavior in the spike. Respect no-transform, HEAD/bodyless statuses, ranges
   and unsupported encodings. Strip internal hint headers on every outgoing path.
3. Treat upstream Content-Length as exact uncompressed length only for a trusted
   unencoded representation. An encoded length is never MLEN and does not prove
   that the origin buffered the body. Verify transparent decompression and header
   behavior with the selected runtime; maintain an explicit representation state.
   The Q3 spike tests requesting `Accept-Encoding: identity` from the origin.
   That avoids origin compression and edge decompression, and yields a trusted
   length when the origin honors it.
4. Once admitted, set Content-Encoding, merge Vary, remove encoded-length and
   other invalidated representation metadata, and weaken strong ETags as needed.
   For `br`, return a streaming Response with `encodeBody: "manual"`. M8 adds
   dcb framing/identity and its Vary/negotiation rules separately.
5. Pump bounded blocks with policy-driven flushes for streaming responses and a
   deterministic byte cap for completion buffering. Await output capacity;
   cancel origin reads and dispose the encoder if the client cancels.
6. Finish and release resources. Shed optional aggregate performance recording
   first. `waitUntil` may manage lifecycle/recording, but does not provide another
   CPU thread or permission to retain bodies for training.

## Commitment and failure states

| State | Allowed behavior on failure |
| --- | --- |
| **Preparing**: neither outgoing headers nor encoded bytes committed | Select the compatible q2-class reference path or an acceptable origin/identity representation, with matching headers and reservation; otherwise return an error |
| **Streaming**: Response handed to runtime or encoded output written | Keep encoding and stream state. At a healthy completed block boundary, optionally continue with bounded uncompressed meta-blocks inside the same br stream. Pending commands/blocks must finish validly before that transition |
| **Completed**: valid encoder EOF written | Release state; optional recording may be shed |
| **Aborted**: state/input unrecoverable or request terminated | Error/cancel the body, cancel upstream, release state. Do not synthesize valid EOF, identity suffixes or a fresh compressed stream |

For bounded precommit probing, retain only the bytes allowed by the configured
cap and replay them into the chosen encoder. Returning the Response is treated
conservatively as commitment even if no body bytes have been written yet. A
hard runtime CPU/memory termination is not a catchable fallback guarantee;
precommit reservations/work limits and healthy boundary continuations reduce
risk but do not promise recovery from termination.

Test model rejection, first-write failure, recoverable budget checks, faults
within literals/copies, block boundaries, truncated origin input and downstream
cancellation. The initial per-flush path can preserve a healthy encoder at block
boundaries; the parked open-block path needs its own demonstrated continuation.

## Remaining M0 architecture choices

- Fork packaging and injection surface (Q2), shell/JS-wasm buffer interface (Q7).
- Model distribution via bundled blob, Static Assets, R2 or KV (Q4), with bounded
  loading/refresh and response version pinning.
- Numeric shared/per-stream/pool limits and runtime headroom from the spike;
  explicit callback/error surface for commitment, backpressure and cancellation.
- Destination for aggregate operational telemetry, excluding training records.
- Public source/span policy, chronological split and route namespace below the
  required tenant/site partition. Live session identity is unnecessary (D-013).
