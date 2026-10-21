# Plan

**This is a living document.** Milestones will be re-scoped, re-ordered, split,
or added as planning conversations and findings come in. That churn is
expected; what is *not* allowed is silent change. Scope changes get a
decision-log entry. Progress is reflected here by checking boxes and updating
status lines as work lands.

Check a box only when the item is done and verified. Partially done items stay
unchecked, optionally with a note.

**Status legend:** `pending` · `in progress` · `done` · `parked`

## M0: Plan the plan  `in progress`

Goal: turn [design.md](design.md) and the kickoff choices into a settled
feature matrix, architecture and milestone ladder. This happens through
planning conversations with the project owner, plus targeted research and
spikes where a decision needs evidence.

- [x] Repo scaffolding for the AI-directed workflow (this scaffold).
- [ ] Feature triage: walk [features.md](features.md) with the owner. Promote
      or reject every `proposed` row, answer the open questions, and record
      significant calls in decisions.md.
- [ ] Research (open Q1): read the brotli decoder sources (google/brotli
      `dec/`, Chromium's brotli source stream, Firefox and WebKit content
      decoding) for partial-output behavior mid-meta-block, and for the
      trailing-whitespace padding question. Output: a short findings note plus
      a D-entry confirming or demoting the one-meta-block layout.
- [ ] Research: current browser brotli window caps with and without a CDT
      dictionary (design verification list). Check current sources, not
      memory.
- [ ] Spike (open Q2): rust-brotli encoder structure. Find where command
      generation and meta-block writing split, and whether a q11 parse can be
      captured and re-injected. Output: fork-mechanics recommendation.
- [ ] Spike (open Q3 + Q7): a throwaway Worker that streams small
      manually-encoded `br` flushes (reference encoder, timed writes) to a
      client capturing arrival times over HTTP/2 and HTTP/3. Output: does the
      edge preserve flush boundaries; JS shell vs workers-rs recommendation.
      Reuse dictionary-worker's shape.
- [ ] Corpus plan (open Q8 in features.md): pick two or three public
      templated sites, a crawl politeness policy, target responses per route,
      and the on-disk corpus format.
- [ ] First full draft of [architecture.md](architecture.md), including
      Workers state placement (open Q4).
- [ ] Toolchain decisions: Rust edition/MSRV, workspace layout, wasm build
      path (wasm-bindgen / workers-rs, wrangler), test stack, fmt/clippy
      settings, CI (or local-only), license audit (`cargo-deny`). Record in
      decisions.md.
- [ ] Rewrite the provisional ladder below into real milestones with exit
      criteria.

**Exit criteria:** the owner has walked features.md and says the plan is good
enough to build from. The architecture-blocking open questions (Q1 to Q4) are
answered or have a named fallback. The toolchain is decided, and M1+
milestones have scopes. M0 is a conversation, not a phase: it exits on the
owner's call, not on a checklist reaching zero.

## Provisional milestone ladder  `pending (to be rewritten in M0)`

Adapted from design.md's milestone list and ordered by risk: measure the
opportunity and prove the substrate before building breadth. This is a sketch
only, so do not start work from these entries. They freely reference
`proposed` features.md rows, and nothing here pre-empts the M0 triage.

- **M1: Workspace, corpus and the go/no-go number.** Cargo workspace,
  fmt/clippy/test, license audit. Crawler and corpus format, baseline (q1/q11
  size + CPU, cross-checked once against C brotli), and the chunk-overlap
  analyzer. Plus a wasm32 build of the reference encoder running in a minimal
  Worker so the substrate risk (wasm throughput vs CPU budget, open Q5) is
  measured early. **Exit:** overlap numbers per route on the corpus, and an
  owner go / change-the-design call.
- **M2: Command-list injection.** Forked encoder accepts an external command
  list. Validate by round-tripping q11's own parse to byte-identical size.
  Round-trip verifier and stream inspector (pending triage) land here because
  every later milestone measures with them.
- **M3: Offline parse cache.** Chunker, anchored cache entries, stitcher, fast
  hasher for misses, then cut rules. Two-pass corpus run: hit rate and size
  vs q1/q11.
- **M4: Route model.** Offline training of context maps and priors;
  prior-smoothed tables at flush; small-block policy. Size gain with and
  without the parse cache.
- **M5: Streaming layout.** One-meta-block mode (if Q1 confirms) or the
  per-flush fallback, with committed MLEN, alignment lottery and
  small-first-flush. Measured on a flush replay harness (pending triage).
- **M6: Worker compressing proxy.** wasm core in the Worker, mode selection,
  same-tick coalescing, finalization, model blob loading, safe fallback
  (pending triage). Measured against a real origin: CPU per response,
  delayed bytes, size.
- **M7: Fleet model and warm cache.** Versioned blob from the trainer CLI,
  cold-start quality on a fresh isolate, cross-isolate determinism check.
- **M8: CDT static variants.** Sidecar, chunk dedup, localized search, parse
  recovery from `.br`. Generation time and size vs full q11-with-dictionary.
- **M9: Generated site dictionary.** Built from admitted chunks, served via
  `Use-As-Dictionary` from the Worker; dictionary-relative cache references.
