# Vision

## What this is

Dynamic HTTP compression is stateless: every response re-runs match finding,
parse decisions and entropy modeling from scratch. Because q11 is far too slow
for that, most origins and CDNs settle for brotli q1 to q5 and leave 15 to
25% of HTML size on the table. Brotli's decoder does not care how the encoder
chose its commands and tables. An encoder that *remembers* those decisions
across responses from the same templated site could emit q11-class output at
close to q1 cost.

chunky is a research implementation of that idea, specified in
[design.md](design.md). It has three parts:

- **A Rust encoder library** built on a fork of rust-brotli. It contains a
  content-defined parse cache, pre-trained per-route entropy models, and a
  streaming meta-block layout for flush-heavy documents.
- **CLI tools** for building and measuring it. They crawl a corpus, run
  baselines, measure chunk overlap, encode, and train models and warm caches.
- **A Cloudflare Worker** that runs the encoder as wasm in a compressing proxy
  in front of an origin. This is the server deployment where the design's
  streaming and learning claims get tested under real request shapes.

The secondary target, cheap CDT dictionary variants of static assets and a
generated site dictionary, builds on the same chunk index.

## Who it's for

1. **The project owner, as a research vehicle.** The goal is numbers that
   decide whether memoized encoding is worth pursuing, and which mechanisms
   carry the gain.
2. **CDN and server compression implementers.** The measurements and the
   design should be transferable to nginx, Envoy or CDN-edge encoders even if
   chunky itself is not adopted.
3. **Site operators with templated dynamic pages**, eventually, via the Worker
   as a drop-in proxy.

## Success criteria

Each criterion is a measurement on a named corpus snapshot against the
reference rust-brotli encoder (cross-checked once against C brotli), on the
same machine, for the same inputs, window and explicit logical flush schedule.
Use q1/q4/q11 baselines. Train only on approved public data, freeze the model,
cache and cut rules, and evaluate later held-out captures (including a template
change). Exact-response replay is labeled separately as a diagnostic.

- **Opportunity measured.** On the public-crawl corpus, the repeatability
  funnel per site and route is reported: cross-response LZ77 +
  static-dictionary repeatability, naive ~256-byte CDC whole-chunk overlap,
  64/128/256-byte and delimiter-biased CDC overlap, and ~256-byte CDC with
  prefix/suffix extension and frozen cut rules. This measures reuse potential;
  legal current-response reference coverage is reported separately. Ordinary
  `br` cannot copy bytes from another response merely because the server has them.
- **Gap recovered.** With a warm parse cache and a trained route model,
  output size on dynamic HTML closes most of the q1→q11 gap. The fraction
  recovered by each mechanism (parse cache alone, model alone, both) is
  reported, as is CPU per byte as a multiple of reference q1, per-route size
  regressions, and comparison with q4. Report gap recovery as
  `(q1_bytes - chunky_bytes) / (q1_bytes - q11_bytes)` when the denominator is
  positive; otherwise report absolute sizes. Warm or stale models can regress.
- **Always valid.** 100% of outputs on the corpus and under fuzzing decode
  byte-identically with an unmodified reference decoder, including cold-cache
  and generic-model runs. Ordinary `br` uses an RFC 7932 decoder; CDT `dcb`
  uses an independent Shared Brotli decoder with the negotiated dictionary.
- **Streams in practice.** In the Worker, streaming-critical responses deliver
  bytes available at each logical flush without waiting for later origin bytes
  in the per-flush layout. Record CPU, total withheld bytes and client-visible
  delivery latency, including long unmatched tails and slow consumers. Worker
  reads are transport fragments, not evidence of application write boundaries.
- **Deterministic.** Identical bytes, logical flush positions, route, model/cache
  versions, dictionary and encoder options produce identical bytes natively
  and in wasm. Input fragmentation alone does not change output.
- **Bounded Worker resources.** Concurrent slow streams stay within an explicit
  isolate-wide allocation budget with runtime headroom. Capacity rejection
  happens before commitment; cancellation releases stream resources.

M1 reports the baseline and the owner sets numerical size, CPU, latency and
memory targets before M3/M4 results are evaluated (D-015). The research goal
of q11-class size near q1 CPU is a hypothesis, not a per-response guarantee.

## Non-goals

- **A custom content-encoding or decoder change.** Use existing `br` and, only
  for capable clients, standardized `dcb` (D-012).
- **A zstd encoder.** zstd lacks the static dictionary, context modeling and
  shared-window meta-blocks the design depends on (design.md: Format choice).
- **Build-time static compression.** Sites with a build already produce q11
  there (design.md: Non-goals).
- **Beating q11 on size.** The value is the CPU asymmetry, not a lower size
  floor.
- **Production hardening for third-party traffic** at this stage. This is a
  research project (D-006). Serving real users' sites would reopen the
  process decision.
- **Logged-in or private-page corpora.** The corpus is logged-out public
  crawls only (D-007).
- **Live private-traffic learning or content padding.** Approval of public
  training data is required; frequency and secret-token detection are heuristics,
  not privacy proofs (D-013). The encoder never appends decoded whitespace.
