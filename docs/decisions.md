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

## D-009: Admission gate is the single privacy boundary; secret slots are literal-only  (2026-10-21, status: accepted)

**Decision.** Response-derived content (parse-cache entries, generated
dictionary contents, offline training samples) enters shared state only after
being seen across N distinct sessions. Private responses (`Cache-Control:
private`, `Set-Cookie`, request `Authorization`) tighten the gate, and
multi-tenant deployments partition state per tenant. Secret slots (from
template hints, or from high-entropy token detection when there are no hints)
are emitted as literals and are never indexed or used as match-verification
targets.

**Context.** From the design doc's Security and privacy section and decisions
table. A shared cache creates timing and BREACH-style channels that stateless
compression does not have.

**Consequences.** Every new path that writes shared state must go through the
gate, with no side doors for "just the model" or "just stats". The value of N
and how it varies with privacy signals are still open (features.md). In the
Worker deployment, "distinct sessions" needs a concrete definition that works
without storing user identifiers.

**Reopen if.** A formal analysis shows a cheaper boundary with equal
guarantees, or the gate makes hit rates on the target corpus too low to matter.

## D-008: Hot path reads; finalization records; learning computes off-request  (2026-10-21, status: accepted)

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

## D-007: Measurement corpus comes from logged-out public crawls, stored outside the repo  (2026-10-21, status: accepted)

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
human directs, decides, and is the only committer. Process weight is lean:
one agent, one pass, human scan, and reviews only when the human asks for one
([workflow.md](workflow.md)).

**Context.** Stated by the project owner at kickoff. This is a research
project with no production users or SLA.

**Consequences.** Agents never commit. The docs are the project's memory and
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

## D-003: Rust workspace; encoder forked from Dropbox rust-brotli  (2026-10-21, status: accepted)

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

## D-002: Emit standard brotli only; no decoder, spec, or content-encoding changes  (2026-10-21, status: accepted)

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

## D-001: docs/design.md is the accepted baseline design  (2026-10-21, status: accepted)

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
