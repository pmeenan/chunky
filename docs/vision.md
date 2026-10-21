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
same machine, for the same inputs.

- **Opportunity measured.** On the public-crawl corpus, the fraction of each
  response's bytes that are byte-identical at ~256-byte chunk granularity to
  an earlier response on the same route is reported per site and route. This
  is the go/no-go number for the design.
- **Gap recovered.** With a warm parse cache and a trained route model,
  output size on dynamic HTML closes most of the q1→q11 gap. The fraction
  recovered by each mechanism (parse cache alone, model alone, both) is
  reported, as is CPU per byte as a multiple of reference q1.
- **Always valid.** 100% of outputs on the corpus and under fuzzing decode
  byte-identically with an unmodified reference decoder, including cold-cache
  and generic-model runs.
- **Streams in practice.** In the Worker, streaming-critical responses deliver
  each origin flush to the client without waiting for later origin bytes,
  apart from the bounded byte-alignment delay the design allows. Per-flush CPU
  and the delayed-byte distribution are reported.
- **Deterministic.** Identical input with identical model and cache versions
  produces identical bytes natively and in wasm.

## Non-goals

- **A new content-encoding or decoder change.** Only standard brotli is
  emitted (D-002), so clients need nothing new.
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
