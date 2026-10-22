# M1 report: baselines, decomposition, funnel and wasm

> **Status: final (2026-10-22).** The owner's M1 exit call is D-028: go, the
> suggested Pareto targets, M3 before M4, dcb stays in M8, and production
> Worker CPU moves to M6.

All numbers here are reproducible from the commands shown, on local data
under `data/` (never committed, D-007). Raw outputs land in
`data/reports/m1/`.

## Setup

- **Corpus:** `data/corpus/sessions/2026-10-21` (23 Google pages) and
  `2026-10-22` (24 Google, 48 Etsy), extracted by `tools/netlog/extract.py`
  (m1.1). The 2026-10-22 Google interstitial is excluded everywhere, which
  leaves 46 Google pages (48.2 MB decoded; 45 served dcb) and 48 Etsy pages
  (26.5 MB). Two sessions only: every result is **indicative** (D-022 sets
  about 300 responses per route for non-indicative claims).
- **Encoders:** pinned C google/brotli v1.2.1 (`db24a98a`), clang 18.1.3
  `-O2 -DNDEBUG` via the `cc` crate (`brotli-c`; `chunky bench` prints the
  full flag line), and rust-brotli at the fork base `6518a4e` with
  `portable-float`, rustc 1.99.1. Both are driven through the same
  call/flush sequence (`crates/chunky-cli/src/encoders.rs`).
- **Verification:** every output in every table was decoded by the C
  reference decoder (with the dictionary attached for dcb) and compared with
  the input. No failures.
- **Call schedules** (per response):
  - `buffered`: one FINISH call.
  - `served`: FLUSH at the recovered logical flush offsets (Google's served
    meta-block ends; Etsy has none, so it equals buffered).
  - `reads`: synthetic origin reads modeled on RE-007 (58% uniform 1–81 B,
    the rest 4 KiB; median about 70 B, mean about 1.75 KB), FLUSH after
    every read. The worst-case flush policy.
  - `reads-noflush`: the same calls as PROCESS without flushing.

## Reference curve (sizes)

```bash
chunky baseline data/corpus/sessions/2026-10-2{1,2} --out data/reports/m1/curve-w22.jsonl --qualities 0-11 --lgwins 22 --dict
chunky summarize data/reports/m1/curve-w22.jsonl
```

C, lgwin 22, total bytes and ratio to C q11 buffered (rust-brotli is within
0.2% of C at q0–q9 and 0.2–0.5% larger at q10/q11; full table in
`curve-w22.md`):

| Quality | Google buffered | Google served | Google reads | Etsy buffered | Etsy reads |
| --- | --- | --- | --- | --- | --- |
| q0 | 1.435 | 1.507 | 2.570 | 1.569 | 2.596 |
| q1 | 1.351 | 1.430 | 2.505 | 1.448 | 2.471 |
| q2 | 1.215 | 1.216 | 1.335 | 1.321 | 1.465 |
| q3 | 1.194 | 1.193 | 1.258 | 1.294 | 1.356 |
| q4 | 1.152 | 1.153 | 1.216 | 1.232 | 1.309 |
| q5 | 1.091 | 1.097 | 1.157 | 1.157 | 1.227 |
| q6 | 1.080 | 1.084 | 1.142 | 1.144 | 1.213 |
| q9 | 1.062 | 1.062 | 1.119 | 1.125 | 1.194 |
| q10 | 1.008 | 1.021 | 1.107 | 1.014 | 1.147 |
| q11 | 1.000 (8,946,881 B) | 1.010 | 1.093 | 1.000 (3,789,601 B) | 1.120 |

- **Cold-path gap:** q2 on Google's own flush schedule is 20.4% bigger than
  q11 on the same schedule (M0 probe: 20.6%).
- **Flushing cost:** Google's served schedule costs q2+ at most 1%. Flushing
  every synthetic read costs q2 +9.9% (Google) and +10.9% (Etsy), q11 +9.3% and +12.0%.
  q0/q1 nearly double under per-read calls, flushed or not (RE-002).
- **What the origins served:** Etsy's gzip was 5,254,489 B, 1.387× our q11.
  Google's 45 dcb pages were 7,192,953 B including the 36 B dcb headers.

### With Google's dictionary (dcb rows, C only)

Same 45 pages; dictionary configs count the 36 B dcb framing per response.
Ratios to C q11 buffered without the dictionary (8,762,558 B):

| Quality | No dictionary, served | Dictionary, buffered | Dictionary, served |
| --- | --- | --- | --- |
| q1 | 1.431 | 1.351 | 1.431 |
| q2 | 1.216 | 1.215 | 1.216 |
| q5 | 1.097 | 0.823 | 0.828 |
| q9 | 1.062 | 0.805 | 0.807 |
| q11 | 1.010 | 0.766 | 0.771 |

- C v1.2.1 still ignores an attached dictionary below q5 (RE-004 holds for
  the pinned release).
- **Google served 0.821**: 6.4% above our dictionary q11 on Google's own
  flush schedule, between our dictionary q9 (0.807) and q5 (0.828).
- rust-brotli has no Shared Brotli dictionary API at the fork base, so the
  dictionary curve is C only.

## Effective match reach (RE-002)

```bash
chunky reach
```

Random (incompressible) data, so any saving is a match. Both engines behave
identically:

| Quality | 3 × 8 KB as 3 calls | Same, 1 call | Repeat reached, buffered | Repeat reached, 4 KiB flushed calls |
| --- | --- | --- | --- | --- |
| q0, q1 | no match (24,010 B) | matched (about 8,080 B) | 64 KiB, not 256 KiB | none |
| q2–q11 | matched (8,028–8,072 B) | matched | at least 1 MiB | at least 1 MiB |

q0/q1 compress each input call independently and bound a single call's
fragment to under 256 KiB. q2 and up keep a persistent hasher across calls
and flushes. rust-brotli matches C here, which closes RE-002's "check in M1".

On the corpus, rust-brotli q11's copies by distance (`chunky parse-stats`,
buffered, % of decoded bytes) are 32.7% within 4 KiB, 32.7% within 4–64 KiB,
12.2% within 64–256 KiB, 5.8% within 256 KiB–1 MiB and 0.7% beyond (Google).
Etsy: 47.0%, 27.7%, 10.4%, 4.5%, 0%.

## Gap decomposition (q1 → q11)

```bash
chunky baseline <sessions> --out curve-lgwin.jsonl --qualities 2,5,9,11 --lgwins 18,24 --schedules buffered,served
chunky baseline <sessions> --out curve-nolcm.jsonl --qualities 5,9,11 --no-lcm --schedules buffered,served
chunky parse-stats <sessions>
```

C, buffered, ratio to C q11 lgwin 22:

| Factor | Google | Etsy |
| --- | --- | --- |
| q1 (fast path, per-call fragments) | 1.351 | 1.448 |
| q2 (persistent hasher, single table) | 1.215 | 1.321 |
| q5 (block splitting, context modeling allowed) | 1.091 | 1.157 |
| q9 → q10/q11 (Zopfli-style parse, H10 hasher) | 1.062 → 1.008 | 1.125 → 1.014 |
| q11 without literal context modeling | 1.007 | 1.017 |
| q11 at lgwin 18 (256 KiB window) | 1.082 | 1.035 |
| q2 at lgwin 18 | 1.054 × q2 | 1.024 × q2 |
| q11 at lgwin 24 | 1.000 (no page exceeds the 4 MiB window) | 1.000 |
| Static dictionary share of bytes, rust q11 | 1.1% | 3.3% |
| Static dictionary share, rust q2 / q5 | 0.0% / 0.4% | 0.0% / 1.1% |

- The q9 → q10 step carries most of the high-quality gain: 5.4% (Google) and
  11% (Etsy) of q11's size. That is parse quality, not entropy coding:
  turning off literal context modeling costs only 0.7% and 1.7%.
- C q5–q9 never chose literal context modeling on this corpus (identical
  sizes with it disabled).
- A 256 KiB window costs 8.2% on Google's 0.36–2.3 MB pages at q11, so D-024's
  4 MiB retained-history cap matters.
- rust-brotli q2–q5 at lgwin 18 lost 9–20% on pages above 512 KiB
  (RE-009). Fixed in the fork after this report: rust now matches C within
  0.5% per page there.

## Memory baselines (D-025)

```bash
chunky mem <sessions>
```

Per-stream heap peak, served schedule, for a 1 KiB response, the median page
(548 KB) and the largest (2.28 MB). C counts its allocator hooks;
rust-brotli counts the CLI's global allocator.

| Config | C 1 KiB | C median | C largest | rust 1 KiB | rust median | rust largest |
| --- | --- | --- | --- | --- | --- | --- |
| q1 lgwin 22 | 0.02 MiB | 2.18 | 3.03 | 0.07 | 2.24 | 3.08 |
| q2 lgwin 18 | 0.29 | 1.34 | 1.29 | 0.33 | 1.49 | 1.26 |
| q2 lgwin 22 | 0.29 | 8.84 | 8.79 | 0.33 | 8.99 | 8.87 |
| q5 lgwin 18 | 1.33 | 4.19 | 3.81 | 1.13 | 4.33 | 3.41 |
| q5 lgwin 22 | 1.33 | 11.74 | 12.64 | 1.13 | 11.82 | 12.67 |
| q9 lgwin 22 | 32.11 | 44.66 | 45.57 | 32.16 | 44.71 | 45.62 |
| q11 lgwin 22 | 0.72 | 57.77 | 58.08 | 32.75 | 60.81 | 61.29 |

- q9's hasher (H9) costs 32 MiB per stream even for 1 KiB, so q9 is not a
  candidate tier inside D-024's 16 MiB slots. q10/q11 grow with input.
- A q2 stream at lgwin 22 peaks at about 8.8 MiB, mostly the ring buffer and
  hash table; q5 at about 12–13 MiB.
- **Shared dictionary:** C's prepared index for Google's 261,354 B
  dictionary is 2.84 MiB, shared by every stream (prepared for q5, q9 or q11
  alike). Per-stream peak with it attached is unchanged within 1% (q5 12.30
  MiB, q11 58.08 MiB on the largest dcb page).
- wasm memory high-water per encoder is in the V8 section below.

Per-response setup time (create/destroy, a whole 1 KiB response, dictionary
preparation) comes from `chunky bench` below.

## Repeatability funnel

```bash
chunky funnel data/corpus/sessions/2026-10-2{1,2}
```

Chronological, per site, % of decoded bytes of the measured responses.
Reusable-parse potential only: plain br copies can reach only the response's
own history (D-015, D-017), so this is **not** a size ceiling. Excluded spans
(D-022/D-027) are never admitted or counted. The cut rules are fixed in code
with no tuning.

| Site | Warm set | Responses | LZ77 ≥ 64 B | CDC 64 B (+ ext) | CDC 128 B (+ ext) | CDC 256 B (+ ext) | 256 B + delimiters (+ ext) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Etsy | all earlier (one session) | 47 | 87.4% | 74.5% (81.7%) | 69.5% (77.5%) | 63.2% (72.4%) | 63.7% (72.1%) |
| Google | all earlier | 45 | 63.2% | 40.5% (50.4%) | 33.0% (42.4%) | 25.8% (32.6%) | 26.0% (32.8%) |
| Google | prior session only (same queries) | 23 | 70.8% | 46.6% (58.0%) | 38.4% (49.5%) | 29.7% (38.3%) | 30.0% (38.5%) |

- Smaller chunks find much more reuse, and prefix/suffix extension adds 7–12
  points at every size. Delimiter bias at 256 B changes little.
- Google's day-2 pages are all panel repeats of day-1 queries, so the
  "prior session" row is the same-query case, not new-query generalization.
- Excluded spans remove 11.4% of Google's bytes from consideration (D-027).

## wasm

### Byte identity (native vs wasm32)

```bash
chunky baseline <sessions> --out identity-native.jsonl --qualities 1,2,5,9,11 --schedules buffered,served,reads
node worker/bench/harness.mjs identity data/reports/m1/identity-native.jsonl
```

Each wasm build ran in Node 24 (V8) through the same slot exports:

| Engine | Encodes compared (94 pages × 5 qualities × 3 schedules) | Identical |
| --- | --- | --- |
| rust-brotli (`chunky-wasm`, wasm32-unknown-unknown, `portable-float`) | 1,410 | 1,410 |
| C v1.2.1 (`crates/brotli-c/wasm`, wasi-sdk 34, `-O2 -DNDEBUG`) | 1,410 | 1,410 |

### Minimal reference-encoder Worker

`worker/` is a thin JS shell (D-021) over `chunky-wasm`'s slot exports: it
negotiates br from `clientAcceptEncoding`, coalesces already-available reads
before flushing (RE-007), sends `no-transform`, strips `Content-Length`,
weakens strong ETags and falls back to the origin response when no slot is
free. Locally (`npm run dev`, workerd from wrangler 4.152.0) a 1,150,179 B
corpus page came back as 244,802 B of br at q2 and decoded exactly with the C
decoder. Production CPU (`$workers.cpuTimeMs`) needs a deploy, which is the
owner's call.

## CPU

```bash
chunky bench data/corpus/sessions/2026-10-2{1,2} --dict --out data/reports/m1/bench-native.json
hostlock exclusive --label "chunky: wasm bench" -- taskset -c 2 \
  node worker/bench/harness.mjs bench data/corpus/sessions/2026-10-2{1,2} --out data/reports/m1/bench-wasm.json
```

`chunky bench` takes `hostlock exclusive` itself (which also waits for the
15-minute load average to fall below 1.0) and pins to CPU 2 with `taskset`.
Host: Intel i9-11900KF, Linux 7.0.0-38; rustc 1.99.1; C flags `-O2 -DNDEBUG
-ffunction-sections -fdata-sections -fPIC -m64` (clang 18.1.3). Sample: 10
distinct responses spread over the corpus by body hash, 8,447,206 decoded
bytes; 1 warm-up and 10 timed passes per config; MAD was under 0.5% of the
median except one C q4 run (5%). Timing includes encoder create and destroy
per response. Dictionary configs run on the dcb rows of the same sample and
are normalized per MB.

**Native, MB/s (higher is better):**

| Quality | C buffered | C served | C reads | C + dictionary, buffered | rust buffered | rust reads | rust/C time, buffered |
| --- | --- | --- | --- | --- | --- | --- | --- |
| q1 | 590 | 580 | 200 | 586 | 368 | 145 | 1.60 |
| q2 | 312 | 307 | 220 | 316 | 221 | 167 | 1.41 |
| q3 | 274 | 267 | 142 | 277 | 199 | 118 | 1.38 |
| q4 | 162 | 173 | 97 | 179 | 118 | 84 | 1.37 |
| q5 | 112 | 113 | 76 | 95 | 77 | 58 | 1.46 |
| q6 | 97 | 105 | 71 | 85 | 65 | 52 | 1.49 |
| q9 | 35.4 | 34.9 | 29.8 | 36.7 | 27.6 | 24.0 | 1.29 |
| q11 | 1.04 | 1.11 | 1.08 | 1.06 | 1.14 | 1.00 | 0.92 |

- C q11 runs at about 1 MB/s on these pages (961 ms/MB). q2 is 300× faster,
  q5 108×.
- rust-brotli is 1.3–1.6× slower than C at q1–q9 and 8% faster at q11.
- Flushing every synthetic read costs C q2 30% more CPU and q5 33%; q1 2.9×.
- The dictionary costs nothing below q5 (it is ignored there, RE-004) and
  about 16% CPU at q5 (8.91 → 10.55 ms/MB).

**V8 (Node 24.18.1, V8 13.6), wasm32, same sample and slot exports, MB/s:**

| Quality | C wasm buffered | C wasm reads | rust wasm buffered | rust wasm reads | wasm/native time, C | wasm/native time, rust | wasm memory high-water, C / rust (buffered) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| q1 | 443 | 150 | 255 | 103 | 1.33 | 1.44 | 7.4 / 8.7 MiB |
| q2 | 244 | 169 | 173 | 128 | 1.28 | 1.28 | 12.1 / 14.1 MiB |
| q5 | 79 | 52 | 67 | 48 | 1.42 | 1.15 | 18.5 / 19.6 MiB |
| q9 | 34.9 | 27.4 | 31.3 | 26.1 | 1.02 | 1.13 | 49.3 / 52.1 MiB |
| q11 | 0.92 | 0.92 | 0.95 | 0.84 | 1.13 | 1.19 | 59.9 / 66.9 MiB |

- In V8, C keeps a 1.4× lead over rust-brotli at q2 (4.11 vs 5.80 ms/MB)
  and 1.2× at q5. The Worker CPU baseline is the C wasm column.
- The memory high-water is the whole instance after encoding the sample
  (largest page 2.28 MB), so it includes the slot's input and output
  buffers; per-read feeding keeps it lower (C q2 reads: 8.9 MiB).
- Node timings are relative A/B numbers only (D-023). The M0 spike measured
  about 9 ms/MB at q2 in production Workers; production numbers for this
  build need a deploy.

**Per-response setup (D-025), native, median of 200:**

| Quality | C create + destroy | C whole 1 KiB response | rust create + destroy | rust whole 1 KiB response |
| --- | --- | --- | --- | --- |
| q1 | 0.1 µs | 5.5 µs | 0.9 µs | 8.6 µs |
| q2 | 0.1 µs | 6.1 µs | 1.0 µs | 12.0 µs |
| q5 | 0.2 µs | 19.8 µs | 1.7 µs | 44.7 µs |
| q9 | 0.3 µs | 527 µs | 1.8 µs | 543 µs |
| q11 | 0.4 µs | 1,273 µs | 1.8 µs | 1,285 µs |

- Both encoders allocate lazily, so create/destroy is nearly free; the cost
  lands on the first block (hash-table allocation and zeroing). q9's 32 MiB
  H9 table makes even a 1 KiB response cost half a millisecond.
- C `BrotliEncoderPrepareDictionary` on Google's 261 KB dictionary: 2.25 ms
  once (2.84 MiB index); attaching it to a stream is under 0.1 µs.

## Multi-origin sample (D-026)

Built by `tools/corpus/httparchive.py` and `tools/corpus/commoncrawl.py`
(docstrings hold the exact commands) into `data/multi-origin/`:

| Slice | Query / source | Rows | Distinct bodies | Domains |
| --- | --- | --- | --- | --- |
| HTTP Archive top10k | 2026-10-01, mobile, root pages, html/script/css, `rank <= 10000` (42.8 GB billed) | 625,370 | 269,514 (28.3 GB) | 11,533 |
| HTTP Archive random | same filters, `TABLESAMPLE SYSTEM (0.03 PERCENT)` (18.5 GB billed) | 266,261 | 123,700 (11.9 GB) | 150,129 |
| Common Crawl | CC-MAIN-2026-39 and -43: en Wikipedia, Commons, en Wiktionary (about 300 pages each), 5 Shopify stores (one tenant each) | 1,281 | 1,281 | 8 |

- Every query was dry-run first and capped at 50 GB billed. Total spend about
  $1.70 (61.3 GB scanned plus 11.1 GB of export egress); the scratch dataset
  and bucket were deleted after download.
- **Held-out-origin split:** `sha256(registrable domain)[:8] mod 100`: under
  60 train, under 80 tune, the rest heldout. A domain never spans splits.
  Identical library bodies on different domains do (about 7,500 per slice).
- **Body fidelity (RE-010):** 5.1% (top10k) and 7.7% (random) of HTTP
  Archive bodies differ from the served bytes. Measurements use only
  `exact` and `unverified` rows.
- Common Crawl served this host normally on 2026-10-22 (RE-008 updated).

### Reference curve by content type and size

```bash
chunky baseline data/multi-origin/httparchive-2026-10-01/top10k --splits heldout --max-per-bucket 150 \
  --qualities 1,2,3,4,5,6,9,11 --schedules buffered,reads --out data/reports/m1/mo-top10k.jsonl
# likewise random, and the Common Crawl slices without --splits/--max-per-bucket
```

Held-out origins only, up to 150 distinct bodies per type and size bucket
(deterministic spread over body hashes). C, lgwin 22, ratio to C q11
buffered in the same cell; "reads" flushes after every synthetic read.
HTTP Archive top10k (the random slice is within a few points everywhere;
`mo-random.md`):

| Type | Size | q11 / decoded | q1 | q2 | q5 | q9 | q2 reads |
| --- | --- | --- | --- | --- | --- | --- | --- |
| html | < 1 KB | 0.480 | 1.373 | 1.342 | 1.074 | 1.076 | 1.407 |
| html | 1–10 KB | 0.285 | 1.322 | 1.295 | 1.089 | 1.082 | 1.357 |
| html | 10–100 KB | 0.301 | 1.206 | 1.155 | 1.065 | 1.058 | 1.222 |
| html | 100 KB–1 MB | 0.136 | 1.408 | 1.304 | 1.132 | 1.101 | 1.485 |
| html | > 1 MB | 0.101 | 1.615 | 1.417 | 1.153 | 1.104 | 1.666 |
| js | < 1 KB | 0.528 | 1.251 | 1.227 | 1.043 | 1.042 | 1.299 |
| js | 1–10 KB | 0.355 | 1.247 | 1.225 | 1.070 | 1.066 | 1.270 |
| js | 10–100 KB | 0.271 | 1.311 | 1.213 | 1.094 | 1.083 | 1.317 |
| js | 100 KB–1 MB | 0.245 | 1.349 | 1.237 | 1.114 | 1.087 | 1.357 |
| js | > 1 MB | 0.196 | 1.455 | 1.303 | 1.133 | 1.095 | 1.439 |
| css | < 1 KB | 0.399 | 1.396 | 1.357 | 1.056 | 1.056 | 1.496 |
| css | 1–10 KB | 0.200 | 1.409 | 1.351 | 1.080 | 1.075 | 1.449 |
| css | 10–100 KB | 0.152 | 1.423 | 1.308 | 1.111 | 1.084 | 1.477 |
| css | 100 KB–1 MB | 0.121 | 1.498 | 1.382 | 1.160 | 1.106 | 1.576 |
| css | > 1 MB | 0.086 | 1.729 | 1.539 | 1.198 | 1.122 | 1.812 |

- **Small responses:** below 10 KB, q5 is within 4–9% of q11 but q2 is
  23–36% bigger. With little to match, the q2 gap there is the fast path's
  single fixed-cost table and header overhead, which is what the generic
  tier's entropy and block policy targets (D-026).
- **Large responses:** the q2 gap grows with size (24–54% above 100 KB) and
  becomes parse-dominated, as on the Google and Etsy pages.
- rust-brotli tracks C within 0.1% at q2 and 0.2–0.3% at q11 on these slices.
- Common Crawl (`mo-cc.md`): Wikimedia pages are 1.33–1.44× at q2 and
  1.11–1.13× at q9; Shopify stores 1.25–1.38× and 1.10–1.12×.

### Held-out-origin chunk reuse

```bash
chunky generic-reuse data/multi-origin/httparchive-2026-10-01/top10k
```

Share of held-out origins' bytes inside content-defined chunks already seen
on training origins (about 1 GB of training bodies and 256 MB of held-out
bodies per type, distinct bodies, hash and length matched). Reusable-parse
potential for the generic tier's common-code cache, not bytes saved: plain
br cannot reference another origin's bytes.

| Slice | Type | Whole body seen in train | CDC 64 B | CDC 256 B |
| --- | --- | --- | --- | --- |
| top10k | html | 0.4% | 13.2% | 9.5% |
| top10k | js | 0.8% | 14.3% | 10.7% |
| top10k | css | 4.3% | 23.2% | 19.3% |
| random | html | 0.2% | 21.0% | 16.5% |
| random | js | 1.5% | 15.3% | 12.4% |
| random | css | 9.3% | 37.1% | 32.8% |

Whole-file duplicates are rare, and chunk-level reuse is 3–20 times larger,
which supports caching per chunk rather than per file (D-026). The random
slice reuses more than the top 10k, consistent with long-tail sites sharing
themes and frameworks.

## For the owner (M1 exit)

**Decided (D-028):** items 1–4 as suggested below; production Worker CPU
moves to M6; capture continues and slices freeze before M3/M4 evaluation.

M1's exit needs a go/change call, recorded Pareto targets, and two ordering
decisions. The data so far, with suggestions:

1. **Go/change.** The opportunity is confirmed on every source: the q2 cold
   path is 21% (Google), 32% (Etsy), 12–62% (multi-origin, by type and size)
   bigger than q11, at about 300× less CPU. The Google and Etsy numbers rest
   on two sessions, so they stay indicative until more sessions exist.
2. **Pareto targets (suggested, against the C reference curve).**
   - Size: at or below C q9 on each route's own flush schedule (Google
     served: 1.062; Etsy: 1.125), stretching to q10 (1.02).
   - CPU: at or below C q2 plus 50% (native about 4.8 ms/MB; V8 C wasm about
     6.2 ms/MB on this host), per response, including setup.
   - Memory: within D-024's 16 MiB full slot at lgwin 22 (C q2 peaks at
     8.8 MiB, q5 at 12.6 MiB).
   - Latency: no added delay; the logical flush schedule is unchanged.
3. **(a) Route model (M4) before parse cache (M3)?** Suggest **parse cache
   first** for templated sites. On Google and Etsy pages the high-quality
   gain is parse quality (q9→q10 is 5–11%; turning off literal context
   modeling costs 0.7–1.7%), and the funnel shows 26–38% (Google) and 63–72%
   (Etsy) of bytes in 256 B chunks seen earlier. For the generic tier, small
   responses point the other way: there the q2 gap is entropy and header
   cost. One option is M3 for tenant routes with M4's small-response policy
   pulled forward for the generic tier.
4. **(b) dcb with Google's dictionary ahead of M5?** Google's dictionary
   saves 23% at q5–q11 (C) and nothing below q5 (RE-004). Google already
   serves dcb at between our dictionary q5 and q9; our dictionary q11 on
   Google's schedule is 6.4% smaller. That is a large, cheap win only for
   Google-like targets, and it needs C-style dictionary addressing in the
   fork (M8). Suggest keeping it after M5 unless Google is the primary
   deployment target.
5. **Also needed from the owner:**
   - More capture sessions (D-022 cadence), then `slices.py --freeze` before
     M3/M4 evaluation. D-027 (proposed) records the per-site slice rule.
   - The go-ahead to deploy `worker/` for production CPU numbers
     (`$workers.cpuTimeMs`).
