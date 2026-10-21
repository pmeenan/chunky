# Decision log

Newest first. Every entry says what was decided, why, and what would reopen it.
Entries are for choices that are expensive to reverse or that a future agent
might silently undo, not routine implementation calls. Aim for a few per
milestone. Existing entries are never edited into a different decision:
reversing or amending one gets a *new* entry that supersedes it (a status-line
annotation on the old entry is fine). When an entry hangs on a claim about
current technology state, check a current source or run a local experiment.
Training knowledge is stale.

**Reading:** scan the D-NNN headings (or grep) and read only the entries your
task touches. Full read is for structural or cross-cutting work.

**Culling:** the log may be periodically pruned. Superseded or moot entries
whose context no longer informs anything current are deleted outright; git
history is the archive. D-numbers are never reused.

The original design rationale lives in the decisions table of
[design.md](design.md). D-001 adopts it as a whole. Entries below extract the
ones agents must not silently drift from, plus kickoff choices the design does
not cover.

Format:

```
## D-NNN: Title  (YYYY-MM-DD, status: accepted | proposed | superseded by D-MMM)
Decision / Context / Consequences / Reopen if
```

---

## D-018: Initial corpus: manually captured logged-out Google Search netlogs, extracted offline  (2026-10-21, status: proposed)

**Decision (proposed by the owner; accepting it amends D-007 and D-013 for this
one source).**

- The first corpus is logged-out Google Search result pages plus Google's
  published compression dictionary. The owner captures them by hand: run a
  normal logged-out browser session with netlog raw-byte capture
  (`--net-log-capture-mode=Everything`, or chrome://net-export with raw bytes)
  and perform searches. There is no crawler, automation or proxy. The corpus
  is a set of offline files, measured offline, separate from the Worker path.
- An extraction script turns each netlog into per-response manifests:
  request/response headers with cookie values stripped, encoded and decoded
  bodies, per-read arrival timestamps, served encoding, and the dictionary
  hash. Dictionary responses are extracted and stored by hash with their
  `Use-As-Dictionary` metadata, so dcb/dcz bodies can be decoded and
  re-encoded offline. Script work verifies which netlog events carry which
  bytes.
- Logical flush positions come from the served encoded stream. They are the
  decoded offsets where Google's stream reaches an encoder flush boundary (a
  byte-aligned meta-block end for `br`, a block end for zstd-based encodings),
  paired with arrival timestamps. Read sizes are recorded separately as
  transport fragments.
- The owner chooses query terms; no other person's queries enter the corpus.
  Record locale, region and device class with each session. Route =
  `/search` plus vertical and device class. Repeat sessions on different days
  to build the chronological training/tuning/evaluation and template-change
  slices (D-015).
- These responses carry `Cache-Control: private`, `Set-Cookie`, and requests
  carry the logged-out session's site-set cookies (checked 2026-10-21). For
  this approved source, those signals do not make a response ineligible. The
  basis is an owner-controlled logged-out session with owner-chosen public
  queries. Cookie values, CSP nonces, per-response ids and tokens, XSRF values,
  and any other value that varies per response without being page content are
  excluded spans and literal-only. Identify them by diffing across responses
  plus token heuristics before deriving anything. Consent and CAPTCHA pages
  are excluded.
- Raw netlogs contain cookies and other credential-equivalent data. They stay
  under `data/` and are never committed or shared; only the extracted,
  cookie-stripped manifests feed the tools.
- Generalization claims beyond Google need at least one more site. Common
  Crawl monthly snapshots are a fallback source with built-in chronological
  slices.

**Context.** Google Search serves a document dictionary and streams its
responses, so browser netlogs give real flush schedules and a real
dictionary baseline. Manual browsing is not automated crawling, so D-007's
crawler politeness rules (robots.txt `Disallow: /search`) do not govern it.
D-013, however, treats private, Set-Cookie and credentialed responses as
ineligible, which is why this source needs an explicit exception.

**Consequences.** M0 adds an extraction-script spike. M1 needs the netlog
extractor and served-stream flush recovery rather than a crawler for this
source. A manual corpus is small. Size the held-out slices to what can be
captured, and treat per-route results from a few hundred responses as
indicative.

**Reopen if.** The manual corpus is too small to separate training and
evaluation meaningfully, or results fail to generalize to other sites.

## D-017: Encoder and Worker invariants: verified emission, chunk-contained references, retained history, immutable artifacts  (2026-10-21, status: accepted)

**Decision.** This entry consolidates the parts of D-010 and D-011 that
still hold (both are culled; git history keeps the originals) and adds three
rules (items 1, 3 and 5). D-012 still governs the format contract and D-014
the commitment and memory states.

1. **Every emitted reference is byte-verified at emit time.** Before writing
   a copy, the encoder compares its source bytes in retained history (or in
   the negotiated dictionary slice) with the input bytes the copy covers. A
   static-dictionary reference is expanded with its transform and compared.
   On a mismatch, the encoder falls back to local search or literals. Cache
   entries, fingerprints, predicted distances and artifacts are hints only;
   none of them can make the encoder emit wrong bytes. The cost is linear in
   the referenced bytes.
2. **Chunk-anchored references stay inside their target chunk**
   (`offset + length <= len(target_chunk)`). Training splits cross-chunk
   matches at target boundaries. The stitcher may coalesce them again when
   the chunks are contiguous in the current response.
3. **Training parses run over real eligible responses, not synthetic
   composite windows.** Chunks that overlap excluded spans are never
   admitted, and references into excluded spans are dropped from admitted
   entries. Real responses keep the reference structure that actually
   co-occurs. D-010 used composite windows to avoid matching into private
   data; D-013's source/span gate now covers that.
4. **Distance short codes, literal contexts and dictionary wire distances
   resolve at emit time.** Cache entries and recovered `.br` parses store
   logical references and explicit distances. The D-012 geometry converts
   them at each output position.
5. **Retained history is an encoder option separate from the declared
   window.** The stream header's WBITS bounds only the decoder. The encoder
   keeps at most a configured retained-history cap (for example 256 KB for
   documents) and never emits ordinary distances beyond it. D-014
   reservations use the cap, not `1 << lgwin`, and the cap is part of the
   determinism options. Static-dictionary and CDT distances still use
   `max_distance` from the declared window. CDT dictionaries are read from
   the shared immutable slice, never copied into stream buffers.
6. **Runtime artifacts are immutable per version** (model, warm cache,
   dictionary sidecar) and pinned per stream. Upgrade parses and retraining
   happen offline and ship as a new version. `waitUntil` handles lifecycle
   and aggregate counters only (D-013).
7. **An upstream Content-Length is an exact uncompressed length only for an
   unencoded representation.** Workers `fetch()` may transparently
   decompress while leaving the origin's `Content-Encoding` and compressed
   `Content-Length` in place. Re-encoded responses drop `Content-Length` and
   weaken strong ETags (RFC 9110 §8.8.3).

**Context.** Before this entry, D-010 and D-011 contained rules that later
entries had rejected (padding, the 2–4-byte bound, trainer samples) in full
text. Verification was defined only for predicted distances, so a
fingerprint collision or a corrupt artifact could produce a valid stream that
decodes to the wrong bytes. Tying memory reservations to `1 << lgwin` caps an
isolate at a few dozen streams even though document matches rarely need
megabyte distances.

**Consequences.** M2 lands emit-time verification and the retained-history
cap. Fuzzing becomes defense in depth rather than the only correctness
argument, and the artifact loader needs to guarantee memory safety, not
content correctness.

**Reopen if.** Measured verification cost dominates per-byte CPU. Then
restrict verification to chunk copies and adopt hash verification only with a
written collision analysis.

## D-016: C brotli is the performance baseline; the baseline is the reference quality curve; success is a Pareto improvement; the cold path is q2-class  (2026-10-21, status: accepted)

**Decision.** This amends D-003 (C used only as a one-time cross-check), D-014
(cold path) and D-015 (baseline set).

- The CPU baseline is C `google/brotli` at a pinned release/commit, not
  rust-brotli. C is the gold standard deployments actually run. It is built
  with clang at documented flags and driven by a thin harness that issues the
  same `BrotliEncoderCompressStream` call and flush sequence as every other
  encoder under test. rust-brotli at the fork's base commit is measured
  alongside it on every report, so the port's size and CPU differences per
  quality are visible. chunky CPU claims are stated against C. C brotli also
  supplies the with-dictionary baseline (`BrotliEncoderPrepareDictionary`
  exists in the local 1.1.0).
- For Worker numbers, the same pinned C source is also compiled to wasm32
  (as dictionary-worker did with emscripten) and timed in V8 next to the
  rust-brotli and chunky wasm builds.
- Baselines span the quality curve from q0 to q11 (at least q1–q6, q9 and
  q11), on identical bytes and logical flush schedules. Report them buffered
  and with flushes. Report each
  quality's effective match reach as well. q0/q1 match only within the
  current input fragment: never across `CompressStream` calls or flushes, and
  not at long distances even in a single feed (RE-002). Setting the same
  `lgwin` does not equalize them.
- chunky succeeds only if its (size, CPU) point is a Pareto improvement over
  that measured curve on held-out data: smaller output than the reference at
  equal or lower CPU, or lower CPU at equal size. The q1→q11 gap-recovery
  fraction remains a diagnostic. Aggregate it byte-weighted per route and
  report per-response distributions.
- When a corpus response was served with a dictionary, also report the
  served bytes and the reference curve with that dictionary attached (Shared
  Brotli, with dcb framing bytes counted).
- The cold-path, rejected-model and fallback encoder is reference q2-class:
  a persistent hasher that matches across flushes. M1 numbers pick the exact
  quality. Its memory is reserved like any other stream's (D-014).
- M1 decomposes the q1→q11 gap using reference-encoder settings (context
  modeling on/off, q9 vs q11 parsing, lgwin, static-dictionary share). The
  owner uses that breakdown to decide whether the route model (M4) is built
  before the parse cache (M3).

**Context.** A local probe against C brotli 1.1.0 (RE-002) showed that
q0/q1 never match across input calls. Under per-read flushing, q1 therefore
inflates the gap and makes a poor fallback. The vision notes that CDNs
already run q4–q5, so "recovers X% of the q1 gap" can look like success while
losing to q5 at the same CPU. design.md calls context modeling the largest
single part of the gap, yet the original ladder built the parse cache first.

**Consequences.** Baseline/bench tools sweep qualities for both C and
rust-brotli and record the C version, compiler and flags. C brotli (MIT) is a
benchmark tool, not a dependency of the core or the Worker; check the
license metadata of any FFI crate used to call it (D-005). If rust-brotli is
materially slower than C at the same quality, chunky has to make up that
difference too, and the report shows it. The cold path,
fallback reservations and the success criteria in vision.md change
accordingly.

**Reopen if.** q2 costs materially more than q1 per flush in wasm. A
q1-speed path with a persistent hasher could then be built in the fork.

## D-015: Evaluate frozen artifacts on later captures; establish per-flush encoding before single-block experiments  (2026-10-21, status: accepted; baseline set amended by D-016)

**Decision.** Training, tuning and evaluation captures are separate and ordered
in time. Freeze the route model, parse cache and cut rules before evaluating
later responses; include a template-change slice and report duplicate bodies
separately. Two-pass replay of the same responses remains an explicitly labeled
diagnostic. Baselines are reference q1/q4/q11 with matched windows and logical
flush positions, cross-checked against C brotli. Report both reuse potential
and references legal in the current response; cross-response overlap is not a
compression-size ceiling for ordinary `br`.

M1 records the baseline; the owner sets numerical size-gap recovery, CPU,
latency and memory targets before M3/M4 evaluation. Report per-route regressions
and cache-only/model-only/combined ablations. M2 establishes per-flush output;
M4 includes versioned artifact loading; M5 validates streaming before M6 proxy
integration. M7 measures fleet rollout and freshness rather than introducing
the blob loader. Exact-length single-meta-block work follows these measurements
as a parked experiment, not a dependency of M1–M7.

**Context.** Replaying training responses can hide lack of generalization.
An immutable Worker model already needs a working loader before deployment.
Separating the streaming experiment makes parse/model gains independently
measurable without assuming padding or browser delivery behavior.

**Consequences.** Updates the D-001 milestone baseline; see [plan.md](plan.md)
for scopes, verification and remaining M0 questions. No performance target is
claimed achieved by this planning change.

**Reopen if.** Held-out results or deployment measurements justify a different
order or evaluation protocol; retain a separate unbiased evaluation slice.

## D-014: Determinism includes flushes and options; fallback follows stream state; memory is bounded per isolate  (2026-10-21, status: accepted; cold path amended by D-016, reservation basis by D-017)

**Decision.** Amend D-008/D-011 as follows:

- Deterministic encoding takes bytes, ordered logical flush positions, route,
  model/cache versions, dictionary identity and encoder options as inputs.
  Match search uses deterministic work limits. Arbitrary input fragmentation
  does not change output when logical flushes stay fixed. Worker read boundaries
  and event-loop scheduling are not reproducible application flush signals.
- A cold or rejected model selects reference q1 before encoding, with the same
  window and flush policy. Validity is unconditional; warm-model size improvement
  and monotonic improvement are measured hypotheses, not guarantees.
- Before returning headers or writing encoded bytes, fallback may select an
  acceptable identity/origin representation or reference q1. After commitment,
  preserve the encoding and encoder state. At a completed meta-block a healthy
  encoder may continue with bounded uncompressed meta-blocks in the same stream.
  An open command/block must finish validly before that transition. Unrecoverable
  state, truncated origin input or hard resource termination aborts the stream;
  never append identity bytes or a new independent brotli stream.
- Demand growth is an allocation optimization, not a capacity bound. Before
  commitment, reserve each admitted stream's worst-case allowed window,
  parsing/output buffers and JS/wasm transfer allowance from a bounded pool.
  Include shared state, model-loading peaks, allocator/growth peaks and runtime
  headroom in the isolate budget. Await downstream capacity before reading more
  input, and release reservations on completion, failure or cancellation.

**Context.** Immutable models cannot remove dependence on read fragmentation
or clock-based search cutoffs. Once a response is committed, a new compressor
cannot replace its existing bitstream. Many small initial buffers can still
grow beyond the Worker memory limit concurrently.

**Consequences.** The architecture specifies commitment states and budget
accounting. M2/M5 test deterministic fragmentation and valid continuation;
M6 injects faults and runs concurrent slow-consumer memory checks. Numeric
pool limits and headroom are set from the Worker spike, not assumed safe.
Live request bodies, fingerprints and histograms do not enter training (D-013).

**Reopen if.** A different determinism contract or bounded continuation is
needed. Resource-failure aborts must remain distinguishable from valid EOF.

## D-013: Approved public sources and eligible spans govern all learning  (2026-10-21, status: accepted; D-018 proposes a source-specific amendment)

**Decision.** Supersede D-009's N-session privacy claim. Initial learning is
offline and uses only operator-approved public corpus sources with explicit
span exclusions. Private responses (`Cache-Control: private` or `no-store`,
`Set-Cookie`, request credentials) and known secret spans are ineligible.
Neither missing privacy headers nor repeated occurrence proves content public.
Partition artifacts by tenant/site. Frequency across eligible public captures
is an optional usefulness filter, not authorization to share content.

The same gate covers cache entries, model statistics, cut rules, dictionaries
and exported training samples. Keep raw crawl captures local under D-007;
derive trainer inputs only after eligibility filtering. Never export a whole
response through a sampling exception. Worker requests contribute no bodies,
fingerprints, route histograms or other content-derived training records.
Operational telemetry is limited to bounded aggregate performance counters.

Secret hints still force literal-only emission and exclude indexing/match
targets. Token detection remains heuristic. These rules alone do not prove
BREACH resistance: table counts, contexts and boundaries can still depend on
secret bytes. Any later private-serving policy must analyze those channels;
do not claim structural noninterference from literal-only emission.

**Context.** A confidential value can appear in N users' responses, and a
session count does not establish public availability. Whole-response sampling
and shared histograms must not bypass the source/span gate.

**Consequences.** No live learning pipeline or session identifiers are needed
for M1–M7. Offline provenance and filtering precede training. The confirmed
secret-slot feature is a mitigation, not a security proof.

**Reopen if.** Learning from private traffic becomes a requirement and a
concrete authorization and information-flow design replaces this exclusion.

## D-012: Exact decoded bytes, per-flush baseline, and separate br/dcb format contracts  (2026-10-21, status: accepted; D-010 since culled, surviving items in D-017)

**Decision.** Amend D-002 and D-010 items 1/2/3/5:

- Preserve the exact input bytes. Initial streaming uses meta-blocks with
  actual lengths, completed at logical flushes; never pad the decoded body.
  Estimated-MLEN whitespace padding is rejected. Single-meta-block encoding
  is parked until an exact length is available and total delay is measured.
- Commands encode copy lengths before literals, but stopping input and exposing
  partial literal output mid-command is permitted. Copy completion at every
  flush is an encoder policy, not an RFC invariant. Pending command fields,
  unmatched tails and bit alignment all contribute to withheld bytes. There
  is no established universal 2–4-byte delay bound.
- Ordinary responses use RFC 7932 `br`. Optional negotiated CDT variants use
  Shared Brotli under RFC 9841 and `dcb` framing under RFC 9842, including its
  36-byte header and dictionary identity. These are existing formats; neither
  path invents a content-encoding or changes a client decoder.
- At a copy position `p`, let `M = min(p, (1 << lgwin) - 16)` and dictionary
  size be `D`. A CDT dictionary reference to byte offset `o` has wire distance
  `M + D - o`. Static dictionary word ID `w` uses `M + D + 1 + w` (`D = 0`
  for ordinary `br`). The dictionary remains accessible outside the rolling
  window; dictionary plus response need not fit that window.

**Context.** Padding contradicts byte-identical verification and is observable
to byte consumers. A local C-decoder probe returned literals before the copy
distance arrived (RE-001); browser parser delivery is still unverified.
The original CDT formula used uncapped response position, which fails after
the rolling window fills. Sources checked during review:
[RFC 7932 §§5/9.3](https://www.rfc-editor.org/rfc/rfc7932.html#section-9.3),
[RFC 9841 §3.2](https://www.rfc-editor.org/rfc/rfc9841.html#section-3.2),
[RFC 9842 §4](https://www.rfc-editor.org/rfc/rfc9842.html#section-4), and the
[reference decoder](https://github.com/google/brotli/blob/master/c/dec/decode.c).

**Consequences.** Keep D-010's chunk-contained references and logical distance
resolution. Independent round trips use an RFC 7932 decoder for `br` and a
Shared Brotli decoder with the negotiated dictionary for `dcb`. Add fixtures
on both sides of window saturation and validate dcb negotiation/framing in M8.

**Reopen if.** Exact-length single-block experiments prove worthwhile without
changing decoded bytes or violating the delivery target. Content-transforming
padding would require separate explicit scope; it is not an encoder fallback.

## D-008: Hot path reads; finalization records; learning computes off-request  (2026-10-21, status: accepted; amended by D-013/D-014/D-016)

**Decision.** The per-request compression path only reads shared state (parse
cache, route model, dictionary sidecars). The post-stream pass only records.
Anything that computes new shared state (q11 upgrade parses, table rebuilds,
model training) runs off the request. With a cold cache and a generic model
the encoder must still produce valid output at q1 quality or better. Learning
must be sheddable without changing output validity.

**Context.** From the design doc's Learning architecture section. It gives a
fixed per-byte cost and output that does not depend on learning state.

**Consequences.** On Cloudflare Workers there is no background thread. The
off-request tiers have to map onto `waitUntil`, Queues, Cron Triggers, Durable
Objects or the offline CLI tools; see the open questions in
[features.md](features.md). Tests must cover the cold-cache and generic-model
path as a first-class configuration.

**Reopen if.** Measurement shows that some learning step is cheap enough and
valuable enough inline that its exclusion costs more than the predictability
gains.

## D-007: Measurement corpus comes from logged-out public crawls, stored outside the repo  (2026-10-21, status: accepted; D-018 proposes manual netlog capture of Google Search)

**Decision.** The measurement corpus is built by crawling public templated
sites while logged out (no cookies, no credentials). Responses are stored in a
local, git-ignored data directory and never committed. Unit and integration
tests use small synthetic or permissively licensed fixtures.

**Context.** Stated by the project owner at kickoff. The design's first
experiment and every milestone measurement need many responses per route from
real templated sites.

**Consequences.** A crawler/collector CLI is M1 work. Crawls must be polite
(robots.txt, rate limits) and record enough metadata (URL, headers, route
label, fetch time) to replay. Route labels for crawled URLs must be inferred,
because no template hints exist for third-party sites. That makes hint-driven
features measurable only on synthetic data until a cooperating origin exists.
Results published from the corpus report aggregates, not page content.

**Reopen if.** The owner gains a cooperating origin with real traffic, or
crawled data proves unrepresentative of logged-in dynamic pages.

## D-006: AI-developed, human-gated, lean research workflow  (2026-10-21, status: accepted)

**Decision.** AI agents write the code from the project documentation. The
human directs, decides, and gates every commit (committing directly or
explicitly asking the agent to commit). Process weight is lean:
one agent, one pass, human scan, and reviews only when the human asks for one
([workflow.md](workflow.md)).

**Context.** Stated by the project owner at kickoff. This is a research
project with no production users or SLA.

**Consequences.** Agents never commit unless explicitly asked by the operator.
The docs are the project's memory and
must be kept current as part of each change. Correctness of encoder output is
protected by automated round-trip checks rather than by review ceremony.

**Reopen if.** The Worker starts serving traffic for sites with real users. At
that point output-path changes should get mandatory review.

## D-005: Apache-2.0 license; permissive dependencies only  (2026-10-21, status: accepted)

**Decision.** The project is licensed Apache-2.0 (LICENSE already present).
Dependencies must be permissively licensed (Apache-2.0, MIT, BSD-2/3-Clause,
ISC, Zlib, Unicode-3.0 or equivalent). Copyleft dependencies are excluded.
Licenses are checked against each crate's own metadata. Forked code (rust-brotli)
keeps its upstream license notices.

**Context.** The LICENSE file was committed by the owner. Verified 2026-10-21
from crates.io metadata: `brotli` 9.0.0 (dropbox/rust-brotli) is
`BSD-3-Clause AND MIT`, `brotli-decompressor` 6.1.1 is `BSD-3-Clause/MIT`, and
`worker` 0.8.7 (workers-rs) is `Apache-2.0`. All are compatible.

**Consequences.** M1 adds an automated license audit (for example
`cargo-deny`) to the checks. The fork's BSD/MIT notices must survive in-tree.

**Reopen if.** A needed capability exists only under a non-permissive license.

## D-004: Server deployment is a Cloudflare Worker compressing proxy (wasm)  (2026-10-21, status: accepted)

**Decision.** The server deployment is a Cloudflare Worker that sits in front
of an origin, fetches responses, compresses eligible ones with the chunky
encoder compiled to wasm, and returns them with `Content-Encoding: br`. This
takes the place of the design's "nginx module or equivalent" for server
integration.

**Context.** Stated by the project owner at kickoff. Verified 2026-10-21 from
Cloudflare docs: a Worker must set `encodeBody: "manual"` on the Response to
serve already-compressed data without the runtime recompressing it. Isolates
have 128 MB of memory, including wasm allocations. CPU per HTTP request is
10 ms on Workers Free and 30 s by default on Paid (configurable up to 5 min).
The bundle limit is 64 MiB uncompressed. Prior art is the owner's
[dictionary-worker](https://github.com/pmeenan/dictionary-worker)
(Apache-2.0), a transparent CDT-compressing Worker using the same
`TransformStream` + `encodeBody: "manual"` + `waitUntil` shape with
emscripten-built brotli/zstd wasm.

**Consequences.** The core crate must build for `wasm32-unknown-unknown`
without threads or filesystem access. Realistic compression needs the Paid
plan's CPU budget. The in-memory parse cache and model are per-isolate and
bounded by 128 MB. Shared or fleet state needs a Workers storage product, and
which one is an open question. The Worker origin fetch gives the design's "CDN
flushes on every read from the origin" shape directly. Whether the edge passes
our flush boundaries through to the client unbuffered must be verified (M0
spike).

**Reopen if.** The edge rebuffers or recompresses manually-encoded streams in
a way that defeats the streaming layout, or Worker CPU/memory limits make the
design unworkable there.

## D-003: Rust workspace; encoder forked from Dropbox rust-brotli  (2026-10-21, status: accepted; C baseline role amended by D-016)

**Decision.** The core encoder library, the CLI tools and the Worker's wasm
module are Rust, in one Cargo workspace. The Worker uses the same core
compiled to wasm32. The brotli encoder is a fork of Dropbox's rust-brotli
rather than the C reference. Whether the Worker's request handling is a thin
JS shell or workers-rs is an M0 question (features.md open question 7).

**Context.** Stated by the project owner at kickoff. rust-brotli is a port of
the C encoder, including the q10/q11 paths. One language serves the CLIs and
the wasm build without C cross-compilation.

**Consequences.** Reference q1/q11 baselines should be cross-checked against
the C `google/brotli` CLI at least once, so that port differences don't skew
the yardstick. Exactly how to fork (vendored crate, git dependency, or
upstreamable patches) is an M0 question.

**Reopen if.** rust-brotli's encoder structure makes command-list injection
much harder than the C reference, or its output or performance diverges
materially from C at the same quality.

## D-002: Emit standard brotli only; no decoder, spec, or content-encoding changes  (2026-10-21, status: accepted; amended by D-012)

**Decision.** chunky output is a valid RFC 7932 brotli stream decodable by any
current browser. No new content-encoding, no decoder modifications on the
client, and no changes to the brotli or CDT (RFC 9842) specifications.

**Context.** The design doc's first goal and non-goal. The project's premise
is that a quality level is purely an encoder knob.

**Consequences.** Every encoder-output path is tested by round-trip through an
independent reference decoder, not just our own. Format-edge tricks (committed
MLEN with padding, empty last meta-block, partial-meta-block flushing) need
verification against real decoder behavior before we rely on them. A modified
*decoder* is allowed only as an offline tool (parse recovery from `.br`), never
on the client.

**Reopen if.** Never silently. A format extension would be a different
project.

## D-001: docs/design.md is the accepted baseline design  (2026-10-21, status: accepted; amended by D-012–D-015)

**Decision.** [design.md](design.md) (Oct 21, 2026) and its decisions table
are adopted as the starting architecture. This covers brotli over zstd,
dynamic responses first with CDT static variants second, anchored parse-cache
references, pre-trained route models with prior-smoothed per-meta-block
tables, the one-meta-block streaming layout, two-axis mode selection, and the
fleet model with a warm cache set.

**Context.** Written by the project owner before kickoff.

**Consequences.** Agents treat design.md as authoritative unless a later
D-entry supersedes part of it. Its open questions and verification list carry
into [features.md](features.md). Its milestone list seeds the provisional
ladder in [plan.md](plan.md). Where the design says "nginx module or
equivalent", D-004 picks the Worker.

**Reopen if.** The first experiment (byte-identical 256-byte chunk overlap per
route) comes back low on the target sites. The design itself says it should
change before code is written in that case.
