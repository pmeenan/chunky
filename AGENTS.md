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

- **Standard brotli out, always.** Every byte we emit must decode with an
  unmodified RFC 7932 decoder: no new content-encoding, no decoder or spec
  changes. Any encoder-output change is tested by decoding with an independent
  reference decoder. (D-002)
- **The design doc is the baseline.** [docs/design.md](docs/design.md) and its
  decisions table are the accepted starting architecture. Deviations get a new
  D-entry. (D-001)
- **Rust for the core, CLIs and wasm.** One Cargo workspace. The encoder is a
  fork of Dropbox's rust-brotli. The Worker shell (thin JS or workers-rs) is
  still open. (D-003)
- **The server deployment is a Cloudflare Worker compressing proxy.** wasm
  core, origin behind it, pre-encoded bodies sent with `encodeBody: "manual"`.
  Budget for the Worker limits: 128 MB per isolate including wasm memory, and
  per-request CPU caps. (D-004)
- **Hot path reads, learning writes off-request.** The compression path only
  reads shared state. Learning can be shed without affecting validity, and a
  cold cache still produces valid output at q1 quality or better. (D-008)
- **Admission gate is the privacy boundary.** Nothing response-derived enters
  shared state, generated dictionaries or samples until it has been seen across
  N distinct sessions. Secret slots are literal-only and never indexed. (D-009)
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
| `data/` | Local crawl corpus and derived data. Git-ignored, never committed (D-007) |

The Cargo workspace (core, CLIs, Worker) lands in M1. Update this table when it
does.

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
   and CPU against reference q1/q11 on the corpus, with the command and corpus
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

Milestone **M0 (plan the plan)**: the feature set, architecture, and milestone
ladder are being worked out through planning conversations. See
[docs/plan.md](docs/plan.md). No code exists yet; the workspace scaffolding is
M1. Keep this paragraph short and current when plan.md milestone status changes
(rule 4).
