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

- **M1: Workspace, corpus and the go/no-go repeatability funnel.** Cargo
  workspace, fmt/clippy/test, license audit. Crawler, route-labeling tool and
  corpus format; baseline (`q1`/`q11` size + CPU, cross-checked once against C
  brotli); benchmark report generator; and the chunk-overlap analyzer reporting
  the 4-step repeatability funnel per route (cross-response LZ77 + static-dict
  ceiling, naive ~256 B CDC, 64/128/256 B + delimiter-biased CDC, and ~256 B
  CDC with prefix/suffix extension + simulated cut rules). Plus a wasm32 build
  of the reference encoder running in a minimal Worker so the substrate risk
  (wasm throughput vs CPU budget, open Q5) is measured early. **Exit:**
  repeatability funnel numbers per route on the corpus, wasm baseline CPU, and
  an owner go / change-the-design call.
- **M2: Command-list injection.** Forked encoder accepts an external command
  list, supporting both fast single-table coding and `q11` clustered-table
  coding (as an offline oracle for M3 before M4 lands). Validate by
  round-tripping `q11`'s own parse to byte-identical size. Round-trip verifier,
  stream inspector and initial property tests land here because every later
  milestone measures and validates with them.
- **M3: Offline parse cache.** Multi-resolution chunker, cache entries with
  strictly chunk-contained `chunk-anchored`, `dictionary-anchored` and
  `predicted-distance` references (D-010), stitcher resolving distance short
  codes (`0..15`) and literal contexts at emit time, fast hasher for misses,
  composite admitted-chunk upgrade parses, then cut rules. Two-pass corpus run:
  hit rate and size vs `q1`/`q11` (under both fast tables and `q11` oracle
  tables), plus differential fuzzing of stitched references.
- **M4: Route model.** Offline training of context maps and priors;
  prior-smoothed pruned tables for known-block modes and complete unpruned
  tables for one-meta-block streaming (D-010); small-block policy. Size and CPU
  ablation with and without the parse cache.
- **M5: Streaming layout.** One-meta-block mode (if Q1 confirms) or the
  per-flush fallback, with committed MLEN, `\n` + `distance=1` shortfall
  padding, copy-ending mid-meta-block flushes, byte-alignment lottery (with
  2–4 B tail-copy split fallback) and small-first-flush (D-010). Measured on
  the flush replay harness.
- **M6: Worker compressing proxy.** wasm core in the Worker with demand-grown
  window buffers, mode selection (checking unencoded `Content-Length`),
  `Content-Length` stripping and `ETag` weakening, same-tick coalescing,
  finalization, immutable versioned model blob loading, template-hint header
  stripping and safe fallback (D-011). Measured against a real origin: CPU per
  response, memory footprint, delayed bytes, size.
- **M7: Fleet model and warm cache.** Versioned blob (`chunky-formats`) from
  the trainer CLI, cold-start quality on a fresh isolate, cross-isolate and
  native-vs-wasm determinism check.
- **M8: CDT static variants.** Sidecar builder and zero-copy dictionary
  loading, chunk dedup, localized search, parse recovery from `.br` (resolving
  distance short codes and static-dict offsets; D-010). Generation time and
  size vs full `q11`-with-dictionary.
- **M9: Generated site dictionary.** Built from admitted chunks, served via
  `Use-As-Dictionary` from the Worker; dictionary-relative cache references.
