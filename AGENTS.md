# chunky: a learned brotli encoder for dynamic HTTP responses

chunky is a brotli-format encoder that aims for q11-class output at close to
q1 CPU on templated dynamic HTML and JSON. It does this by remembering parse
and modeling decisions across responses (parse cache, pre-trained route
models, a streaming meta-block layout) instead of recomputing them for every
stream. It ships as a Rust workspace: a library, a set of CLI tools for
building and measuring it, and a Cloudflare Worker (wasm) that runs it as a
compressing proxy in front of an origin. It is a research project. Almost all
code is written by AI agents working from the project documentation, and a
human directs and reviews the work.

**Read this file first, then pull docs on demand via the "Doc map" below. Don't
read everything up front.** This file is long-term project memory and the
rulebook for agents.

## Load-bearing constraints (change deliberately, never silently)

Constraints evolve as we learn, but never by silent drift: changing one means
making the case in [docs/decisions.md](docs/decisions.md) and updating the
affected docs. Until then, these govern.

- **Standard formats, exact original bytes.** Ordinary responses use RFC 7932
  `br`; negotiated CDT uses RFC 9841/9842 `dcb`. No custom formats or decoder
  changes. Every output path round-trips through an independent reference
  decoder to the exact input, without content padding. (D-002, D-012)
- **The design doc is the baseline.** [docs/design.md](docs/design.md) and its
  decisions table are the accepted starting architecture. Deviations get a new
  D-entry. (D-001)
- **Rust for the core, CLIs and wasm.** One Cargo workspace. The encoder is a
  fork of Dropbox's rust-brotli. The Worker shell is thin JS around raw
  wasm exports (D-021). (D-003)
- **The demo deployment is a Cloudflare Worker compressing proxy.** wasm
  core, origin behind it, pre-encoded bodies sent with `encodeBody: "manual"`.
  It is a proof of concept; the goal is direct platform implementations, so
  core limits are tunable knobs with measured costs, and Worker budgets
  (128 MB per isolate, CPU caps) configure only the demo. (D-004, D-029)
- **Hot path reads, learning writes off-request.** The compression path only
  reads shared state. Learning can be shed without affecting validity, and a
  cold cache uses a q2-class reference encoder with the same window and flush
  policy. Every emitted reference is byte-verified, so artifacts cannot cause
  wrong output. (D-008, D-014, D-016, D-017)
- **Learning uses approved sources only, offline.** Shared caches, models,
  dictionaries and samples all pass the same source and span eligibility gate.
  Live traffic never trains them; repetition is not proof of publicity.
  Secret slots are literal-only and never indexed. (D-013; D-019 relaxes
  header-based ineligibility for approved sources; D-026 shares a generic tier
  across tenants)
- **Corpus stays out of the repo.** Crawled responses live in a local data
  directory and are never committed. Tests use small synthetic or
  permissively-licensed fixtures. (D-007)
- **Apache-2.0; permissive dependencies only.** Check each dependency's license
  in its own crate metadata before adding it. Forked code keeps its upstream
  notices. (D-005)

## Repository layout

| Path    | What lives there |
| ------- | ---------------- |
| `docs/` | Design, vision, plan, architecture, decisions, features, rough edges, workflow |
| `data/` | Local corpus (captured netlogs, crawls) and derived data. Git-ignored, never committed (D-007) |
| `crates/` | Cargo workspace (D-023): `brotli` (rust-brotli fork), `brotli-c` (pinned C baseline + reference decoder), `chunky-core`, `chunky-formats`, `chunky-cli` (binary `chunky`), `chunky-wasm` |
| `third_party/google-brotli/` | Vendored C brotli v1.2.1 |
| `worker/` | Cloudflare Worker JS shell, `wrangler.toml`; `worker/bench/` has the V8 wasm harness |
| `scripts/check.sh` | The repo's checks, locally (under `hostlock shared`) and in CI |
| `tools/netlog/` | Netlog extractor (`extract.py`), cross-session spans (`spans.py`), slices (`slices.py`), query panel and Etsy list |
| `tools/corpus/` | Multi-origin sample extraction (HTTP Archive, Common Crawl) |
| `tools/capture/` | Capture-session extension, link page and `deploy.sh` to `chunky.meenan.dev/capture/` (D-022) |

## Doc map: pull what the task needs, not everything

Always read (it's short): [docs/workflow.md](docs/workflow.md). It covers the
build → commit loop, on-demand reviews, and the human commit gate.

| Doc | Read when the task needs |
| --- | --- |
| [docs/plan.md](docs/plan.md) | What to work on, milestone scope, exit criteria (what "done" means) |
| [docs/design.md](docs/design.md) | The full technical design: parse cache, route model, streaming layout, CDT, security |
| [docs/vision.md](docs/vision.md) | Why the project exists, who it's for, success criteria, non-goals |
| [docs/features.md](docs/features.md) | The feature matrix: confirmed scope, proposed additions, open questions |
| [docs/architecture.md](docs/architecture.md) | Crate/Worker structure and how the design maps onto them |
| [docs/decisions.md](docs/decisions.md) | Settled choices (D-NNN). Scan headings; read only the entries your task touches |
| [docs/rough-edges.md](docs/rough-edges.md) | Findings log (RE-NNN). Grep before adding a finding or debugging weirdness |
| [docs/m1-report.md](docs/m1-report.md) | M1 baselines: reference curve, reach, decomposition, funnel, memory, CPU, wasm; exact commands |

## Rules for all agents

1. **Log decisions sparingly.** [docs/decisions.md](docs/decisions.md) is for
   choices that are expensive to reverse or that a future agent might silently
   undo: the load-bearing constraints above, on-disk formats (cache entries,
   model blobs, sidecars), and chunker parameters. Routine implementation,
   naming and scope calls don't get entries. Aim for a few entries per
   milestone, not one per task.
2. **Log findings that cost you.** Add a
   [docs/rough-edges.md](docs/rough-edges.md) entry when a brotli-format,
   rust-brotli, wasm or Workers-runtime quirk burned real debugging time and
   will bite again. Skip the formal reproduction unless it's cheap to capture.
3. **Measure what a decision hangs on.** Compression claims are numbers: size
   and CPU against the reference quality curve (pinned C brotli for CPU) on the
   corpus, with the command and corpus
   named. When a choice depends on current platform behavior (Workers limits,
   browser decoders), check a current source or run it. Training knowledge is
   stale. Everything else: ship it and see.
4. **Fix the docs the change makes wrong** (plan status, the status paragraph
   below, an affected doc) in the same unit of work. Nothing more is owed.
5. **Never commit unless explicitly asked.** By default, agents do not run
   `git commit`/`git push` or rewrite history; changes stay in the working tree
   for human review. Commit only when the human operator explicitly asks you to
   commit.
6. **Rust conventions.** Code must pass `rustfmt` and be `clippy -D warnings`
   clean. Any `unsafe` needs a comment stating the invariant it relies on. The
   core library must build for `wasm32-unknown-unknown`, so keep threads,
   filesystem and clocks behind traits in the CLIs or the Worker.
7. **Keep the always-loaded context lean.** This file is imported into every
   conversation, so every line added costs every future agent. Detail belongs
   in `docs/` behind the doc map, not here.
8. **Scratch files stay out of the tree.** Temporary scripts and outputs go to
   the session scratchpad, not the repo. Corpus data goes in the git-ignored
   data directory. Delete throw-away diagnostics before concluding.

## Current status

**M0, M1 and M2 are done** (2026-10-22). M1's baselines are in
[docs/m1-report.md](docs/m1-report.md); its exit (D-028) set the Pareto
targets against the C curve, put the parse cache (M3) before route models
(M4) and kept dcb in M8. M2 added the fork's verified external-command
writer, the per-flush `Stream` with a q2-class cold path, and the
inspector; its corpus results are in plan.md. **M3 is next:** the offline
parse cache. Capture sessions continue. See [docs/plan.md](docs/plan.md).
Keep this paragraph short and current
when plan.md milestone status changes (rule 4).
