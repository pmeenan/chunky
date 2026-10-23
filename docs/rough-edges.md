# Rough edges: findings log

Brotli-format, rust-brotli, wasm, wrangler and Cloudflare Workers runtime bugs,
quirks, surprising limits, performance cliffs and missing capabilities found
while building chunky. Log the ones that burned real debugging time and will
bite again. This is a save-future-you log, not a compliance artifact.

**Before adding:** grep for the API or library involved to avoid duplicates.
**Before debugging weirdness:** check here first, since it may be known.

A good entry says what environment it happened in and what was observed vs.
expected. Include a reproduction when it's cheap to capture.

Format:

```
## RE-NNN: Title  (YYYY-MM-DD, status: open | fixed-upstream | worked-around | wontfix)
Environment / Repro or measurement / Observed / Expected / Impact / Links
```

Newest first. RE-numbers are never reused.

**Leads from prior art (unverified here; promote to an entry if they bite):**
the owner's [dictionary-worker](https://github.com/pmeenan/dictionary-worker)
read the client's encodings from `request.cf.clientAcceptEncoding` rather than
only `Accept-Encoding`. It also served dictionaries as
`application/octet-stream` because a compressible content type triggered
"something weird". Additionally, Cloudflare Workers' subrequest `fetch()`
transparently decompresses `br`/`gzip` on `originResponse.body` while leaving
the origin's `Content-Encoding` and compressed `Content-Length` headers on
`originResponse.headers` (see D-017: never use `Content-Length` as uncompressed
`MLEN` when `Content-Encoding` is present, strip `Content-Length` on re-encoded
responses, and weaken strong `ETag`s).

---

## RE-012: Native meta-block writers assume every copy emits at least two bytes  (2026-10-22, status: worked-around)

Static-dictionary transforms can legally produce one byte (for example,
four-byte word 0 with transform 23, omit-last-3). Accepting a dense injected
parse of these overflowed the external writer's command scratch, sized for
one command per two bytes. Merely growing that scratch exposed another
assumption: greedy tables read the two preceding bytes after the first
copy, even when only one byte has been produced. The external verifier now
demotes dictionary copies shorter than two bytes to literals, preserving
the native writer's invariants. Regression:
`stream::tests::one_byte_dictionary_copies_round_trip`, checked with C,
Rust and the inspector at table qualities 2, 5 and 11.

## RE-011: Replaying a rust-brotli q11 parse exactly needs each command's distance code, not just its distance  (2026-10-22, status: worked-around)

**Environment.** Fork at `6518a4e` with `portable-float`, lgwin 22, buffered
single feed; M2 `chunky-core` test `captured_q11_parse_replays_exactly`
(synthetic fixtures).

**Observed.** A q11 parse re-injected with q11 tables on its own meta-block
layout came out the same size as the reference stream but with different
bytes on the `mixed` fixture. Zopfli sometimes codes a distance explicitly
(code ≥ 16) even when a short code (0–15) would resolve to it, because it
picks codes by cost. A writer that re-derives codes greedily
(`ComputeDistanceCode`) turns those into short codes. The M0 spike saw the
reverse case: the meta-block callback (`log_meta_block`) reports only final
distances, so short codes chosen by cost were lost too.

**Fix.** The fork's parse log records each command's distance code, and
`Reference::Window::short_code` carries it as a hint: a short code is
honoured when it still resolves to the same distance, and
`EXPLICIT_DISTANCE` (16) forces the explicit form. With that, replay is
byte-identical to reference q11 on every fixture.

**Related upstream bug (fixed in the fork).** rust-brotli's `encode_data`
passed `ringbuffer_.data_mo.slice()` to `ChooseContextMode` without the
`buffer_index` (2) offset, so its q10/q11 UTF-8 test read a window two bytes
earlier than C (`ringbuffer_.buffer_`). The fork now passes the offset slice.
It matters only at the 75% UTF-8 threshold: no M1 corpus page changed at
q10/q11 (`chunky baseline`, both sessions, buffered/served/reads). Regression
test `reference::tests::literal_context_mode_matches_c_at_the_utf8_threshold`;
upstream status in `crates/brotli/CHUNKY.md`.

## RE-010: HTTP Archive `response_body` is not always the served bytes  (2026-10-22, status: worked-around)

**Environment.** `httparchive.crawl.requests`, crawl 2026-10-01, mobile
root pages, `html`/`script`/`css` (`tools/corpus/httparchive.py`).

**Observed.** Comparing `response_body` with HTTP Archive's own hash of the
body as received (`payload._body_hash`):

| Fidelity | top10k rows | random rows |
| --- | --- | --- |
| exact | 430,711 | 225,530 |
| no `_body_hash` (unverified) | 12,768 | 3,408 |
| shorter, tail ends mid-token (truncated) | 12,030 | 10,104 |
| CRLF stored as LF | 11,554 | 8,678 |
| byte-order mark stripped | 185 | 138 |
| capped at about 8.0–8.8 MB (reported 9–19 MB) | 45 | 18 |
| longer, or same size but different | 241 | 137 |

So 5.1% (top10k) and 7.7% (random) of rows with a body differ from the
served bytes. Bodies are stored as UTF-8 STRING, decoded from any
Content-Encoding. An empty body is NULL, and 54,645 top10k status-200 rows
have no body at all.

**Expected.** Exact response bytes.

**Impact.** Every multi-origin manifest row carries `fidelity`, and the
measurement tools use only `exact` and `unverified` rows. Size results for
CRLF-heavy or legacy-charset sites are therefore under-represented. Do not
use HTTP Archive bodies where exact bytes matter without checking
`_body_hash`.

## RE-009: rust-brotli q2–q5 lose 9–20% once input passes the 2^(lgwin+1) ring buffer  (2026-10-22, status: worked-around)

**Environment.** Vendored rust-brotli at the fork base (`6518a4e`,
`portable-float`) vs pinned C google/brotli v1.2.1, lgwin 18, buffered single
feed, 94 corpus pages (two sessions, Google and Etsy). Command:
`chunky baseline <sessions> --qualities 2,5,9,11 --lgwins 18,24`.

**Observed.** rust/C size ratio by page size at lgwin 18:

| Page size | Pages | q2 | q5 |
| --- | --- | --- | --- |
| 256–512 KiB | 45 | 1.000 | 1.000 |
| 512 KiB–1 MiB | 28 | 1.094 | 1.101 |
| over 1 MiB | 21 | 1.201 | 1.206 |

q9 and q11 match C at lgwin 18 (within 0.3%). At lgwin 22 and 24, where no
page wraps the 8 MiB ring buffer, rust matches C at every quality. Both use
the same hasher type (`ChooseHasher`), so the loss starts exactly where the
ring buffer wraps: the H2–H5 path finds fewer matches after the wrap.

**Expected.** rust-brotli is a port and should track C at every window.

**Impact.** D-024's small fallback slot plans lgwin-18 q2-class encoding,
and M2's retained-history cap sits below the ring buffer. Both hit this on
pages over 512 KiB. Fixed in the fork (below); the patch is upstreamable.

**Root cause.** The bulk hash-store fast paths, `StoreRangeOptBasic` (H2–H4,
H54) and `StoreRangeOptBatch` (H5/H6 with 4-byte hashes), wrote the
ring-buffer-masked index `ix & mask` into the buckets, while `Store`,
`FindLongestMatch` and C store the unmasked position. Below the ring buffer
size the two are equal. After the first wrap every bulk-stored entry (the
interior of each emitted copy) reads as `cur_ix - (ix & mask)`, at least one
ring buffer back, which exceeds `max_backward`, so the match is dropped. q9+
use other hashers, and lgwin 22/24 never wrapped on this corpus.

**Fix.** Fork patch in `backward_references/mod.rs`: store the unmasked
position and mask only the data read (`crates/brotli/CHUNKY.md`). Upstreamable
as-is. Integer-only, so native and wasm stay identical, and output is
byte-identical below the ring buffer size (all 180 rust encodes of pages
≤ 512 KiB unchanged). Same baseline command at q2–q5, lgwin 18, buffered,
rust/C size ratio after the fix:

| Page size | Pages | q2 | q3 | q4 | q5 |
| --- | --- | --- | --- | --- | --- |
| 256–512 KiB | 45 | 1.000 | 1.000 | 1.000 | 1.000 |
| 512 KiB–1 MiB | 28 | 1.000 | 1.000 | 1.000 | 1.000 |
| over 1 MiB | 21 | 1.000 | 1.000 | 1.000 | 0.999 |

Worst page 1.005 (q2, pre-existing, below the wrap). Regression test:
`chunky-core` `reference::tests::tracks_c_after_ring_buffer_wrap` (1.25 MiB
synthetic input at lgwin 18; 1.19 at q2 and 1.22 at q5 without the fix).
Status flips to fixed-upstream once dropbox/rust-brotli takes the patch.

## RE-008: Common Crawl's CDX server times out; repeated Parquet index scans get the host throttled  (2026-10-21, status: open)

**Environment.** Common Crawl CC-MAIN-2026-34/39/43, from the build host.

**Observed.**

- `index.commoncrawl.org` returned 504 for wildcard and prefix queries on
  large domains, and intermittently even on narrow ones.
- Querying the columnar index (`cc-index/table/.../*.parquet`) over HTTPS with
  DuckDB 2.0 `httpfs` worked well at first. Each query read the footers and
  row-group statistics of all 300 files and pruned by `url_surtkey` in about
  2 s.
- After roughly 30 such queries (8 threads, then 2), `data.commoncrawl.org`
  answered every request with a CloudFront 403, including a single 1 KB range
  request.

**Impact.**

- For site discovery, read `cluster.idx` and range-fetch only the matching
  CDX blocks, or cache the needed Parquet row groups locally once. Don't
  re-scan all 300 files per question.
- Keep WARC record fetches sequential and paced.
- A 403 from the CDN means back off for hours. Never route around it.
- 2026-10-22: the `cluster.idx` + CDX block approach with 1 request per
  second ran about 1,500 requests and two 105 MB `cluster.idx` downloads
  without throttling (one 504, recovered after a 30 s back-off)
  (`tools/corpus/commoncrawl.py`).

## RE-007: Origin fetches ignore `Accept-Encoding: identity`; Workers get decoded bodies in tiny reads  (2026-10-21, status: open)

**Environment.** Route Worker on `chunky.meenan.dev/*` (no compatibility
flags) fetching the same host, which reaches the owner's nginx origin
(`gzip on`, no brotli module). Body: a 533,659 B static synthetic HTML page.

**Observed.**

- With the origin request's `Accept-Encoding` set to `identity`, `gzip`,
  `br`, `gzip, br` or left unset, nginx's access log showed 88,696 B sent
  every time, which is the gzip size. So the origin always received a
  gzip-capable `Accept-Encoding`.
- The Worker saw `Content-Encoding: null` and `Content-Length: null`, and
  decoded bytes in about 305 reads per page: median 70 B, minimum 1 B,
  maximum 4,096 B. All of them were available at once (`Date.now()` did not
  advance across the loop).
- Flushing every read cost +12.5% at q2 (127,861 vs 113,611 B) and +8.8% at
  q5 (106,081 vs 97,510 B).
- Flushing only when the next read was not already available, tested by racing
  the pending `read()` against `setTimeout(0)`, gave 0 flushes on this page and
  the unflushed size. On a paced origin (500 ms gaps) it kept per-marker
  latency identical to per-read flushing, with 5 flushes for 11 reads.

**Expected.** That an identity request yields an uncompressed origin response
with a trusted `Content-Length`. The prior-art lead above also expected
`Content-Encoding` to stay visible.

**Impact.**

- Plan on no trusted uncompressed length from origins behind Cloudflare.
  The parked exact-length experiment needs another source of `MLEN`.
- Read sizes are an artifact of decompression, so per-read flushing is a poor
  default. The coalescing policy is the initial default.
- Recheck when the compatibility date or origin compression changes.

## RE-006: Workers Cache can serve a cached dcb body to a client that did not advertise dcb  (2026-10-21, status: open)

**Environment.** Workers Cache (`[cache] enabled = true`, wrangler 4.151.3) on
a route on `chunky.meenan.dev`, Worker-generated bodies with `encodeBody:
"manual"`, `Cache-Control: public, max-age=300, no-transform`,
`Vary: accept-encoding, available-dictionary`. The Worker chose dcb only when
`request.cf.clientAcceptEncoding` listed dcb and `Available-Dictionary`
matched.

**Observed.**

- Clients that advertised dcb with the matching dictionary got
  byte-identical cached dcb, and a different dictionary got br.
- Identity-only clients got the decoded br variant.
- A client that sent the matching `Available-Dictionary` with
  `Accept-Encoding: gzip, deflate, br, zstd` (no dcb) got a cache **HIT on the
  dcb variant**. So did one sending `Accept-Encoding: br`. This happened
  whichever order the variants were cached in.

**Cause.** Cloudflare rewrites the request's `Accept-Encoding` to `gzip, br`
before the cache key is built (RE-005). `Vary: Accept-Encoding` therefore never
separates variants, and only `Available-Dictionary` does.

**Impact.** Browsers pair `Available-Dictionary` with dcb in
`Accept-Encoding`, but other clients need not. Cache dcb only behind an outer
entrypoint that rewrites the request's encoding headers from
`clientAcceptEncoding` before the cached entrypoint. Otherwise mark dcb
responses uncacheable at the edge.

## RE-005: Cloudflare transcodes Worker-encoded br unless `no-transform`; Workers see a normalized Accept-Encoding  (2026-10-21, status: worked-around)

**Environment.** Proxy Worker on `chunky.meenan.dev/*` and on `workers.dev`,
compatibility date 2026-09-01, returning rust-brotli br with `encodeBody:
"manual"`. Clients: curl 8.5 (HTTP/2) and headless Chrome 156 (HTTP/3).

**Observed.**

- Without `Cache-Control: no-transform`:
  - A client accepting zstd (Chrome's `gzip, deflate, br, zstd`) received
    **zstd**: the edge decoded our br and re-encoded it.
  - An identity-only client received decoded bytes.
  - `br` alone passed our stream through. Its byte-aligned flush boundaries
    sat exactly at the cumulative origin-read offsets.
- With `no-transform`, zstd-capable clients received our br byte-exact,
  including from Workers Cache.
- `request.headers.get('accept-encoding')` was always `gzip, br`, even for an
  identity-only client.
- `request.cf.clientAcceptEncoding` carried the client's value, keeping `dcb`
  and `dcz` but dropping `zstd`. `Available-Dictionary` passed through
  unchanged.
- A Worker `fetch()` to another `workers.dev` Worker failed with error 1042
  until the `global_fetch_strictly_public` compatibility flag was set.

**Impact.** Every encoded response sends `no-transform`. Negotiation reads
`clientAcceptEncoding`, never the request header. The edge still decodes for
identity-only clients if the Worker gets that wrong, so negotiation errors fail
safe for br but not for dcb (RE-006). The prior-art lead's "something weird"
with compressible content types is probably this transcoding.

## RE-004: C brotli ignores attached dictionaries below q5; rust-brotli uses its prefix dictionary from q2  (2026-10-21, status: wontfix)

**Environment.** System C brotli 1.1.0 (`libbrotlienc.so.1`) via ctypes,
`BrotliEncoderPrepareDictionary(BROTLI_SHARED_DICTIONARY_RAW)` plus
`BrotliEncoderAttachPreparedDictionary`, lgwin 22. Corpus: 23 Google `/search`
pages (24.2 MB) with Google's 261 KB search dictionary (M0 opportunity probe).

**Observed.** Total compressed bytes with vs without the dictionary:

| Quality | Without dictionary | With dictionary |
| --- | --- | --- |
| q2 | 5,559,915 B | 5,560,338 B |
| q5 | 5,013,758 B | 3,808,663 B |
| q11 | 4,613,409 B | 3,558,668 B |

This is by design. C 1.1.0 only searches an attached dictionary from q5 up.
That is still true in every release through v1.2.1 (2026-10-21). Master lowered
it to q3 in `54f74c7f` (2026-02-13): `c/enc/backward_references.c` searches
the compound dictionary for hashers H3–H6, except q4 with a size hint of at
least 1 MiB (H54) and q3 with lgwin above 24 (H35). q2 never searches it.

rust-brotli at ToT (`6518a4e`) has no shared-dictionary or dcb API. Its
custom LZ77 (prefix) dictionary is skipped at q0/q1 and used from q2 up (q10
and q11 since `e380aa5`). Checked 2026-10-21 on 3 corpus pages (356 KB to
2.3 MB) at q2/q5/q11 with lgwin 22 and 18:

- All 18 rust-brotli outputs decode exactly with C libbrotlidec when Google's
  dictionary is attached as RFC 9842 dcb would, so the streams are
  dcb-compatible.
- rust-brotli's own decoder with that dictionary decodes all 22 of Google's
  served dcb streams (`flushscan`).
- At lgwin 22, q2 uses the dictionary (`youtube`: 62,884 B vs C's 109,878 B),
  and q5/q11 land within 0.1–3.6% of C.
- The dictionary sits in front of the window, so it slides out of reach as the
  response grows. C's compound dictionary stays addressable. At lgwin 18,
  `youtube` q11 is 75,645 B vs C's 47,306 B.

**Expected.** That a dictionary helps at every quality.

**Impact.** The with-dictionary reference curve (D-016) only exists at q5 and
above for C 1.1.0. Report the version and its cutoff with that curve.
chunky's own encoder is not bound to brotli's quality levels, so tuned
dictionaries can be used down to the q1-class path. The fork can build dcb on
rust-brotli's custom dictionary. M8 needs C-style addressing beyond the window
(D-012's capped `max_distance` geometry) so large responses keep the
dictionary in reach.

## RE-003: Default rust-brotli output differs between native and wasm32 at q11  (2026-10-21, status: worked-around)

**Environment.** dropbox/rust-brotli 9.0.0 (`6518a4e`), rustc 1.99.1,
`x86_64-unknown-linux-gnu` vs `wasm32-unknown-unknown` run in Node 24 (V8),
default `std` features, lgwin 22.

**Repro.** Compress 8 inputs (rust-brotli testdata plus synthetic HTML) at
q2/q5/q9/q11 with `brotli::BrotliCompress` in a native binary and in a wasm32
cdylib. Then compare the bytes.

**Observed.** 2 of 32 pairs differ, both at q11: `alice29.txt` (46,493 vs
46,492 B) and `random_then_unicode` (124,674 vs 124,662 B). Both decode
correctly. With `--features portable-float`, 0 of 32 differ. Native default
vs native `portable-float` differ on 5 of 32.

**Expected.** Identical bytes for identical input and options (D-014).

**Impact.** The `std` cost estimators call libm `log2`/`powf`
(`src/enc/util.rs`), and native glibc and wasm libm round differently.
`portable-float` switches to the table path that `no_std` builds already use.
D-020 makes it mandatory for every measured or shipped build. Keep it on for
the reference rust-brotli builds too, or native/wasm comparisons will drift.

## RE-002: Reference q0/q1 never match across input calls or flushes  (2026-10-21, status: open)

**Environment.** Local `libbrotlienc.so.1` (C brotli 1.1.0) via Python ctypes,
`lgwin = 22`. rust-brotli is a port and probably behaves the same; check in M1.

**Repro.** Generate 8000 random bytes `B`. Feed `B`, `B`, `B` as three separate
`BrotliEncoderCompressStream` calls: first with `FLUSH` after each piece, then
with plain `PROCESS` calls, then once as a single 24000-byte feed. Also compress
a 300 KB random blob repeated twice in a single feed.

**Observed.**

| Quality | Three calls, with or without FLUSH | Single 24 KB feed | 300 KB blob ×2, one feed |
| --- | --- | --- | --- |
| q0, q1 | 8003 / 8003 / 8004 bytes, no matches | 8071 bytes (matches found) | 600005 bytes (no match) |
| q2 | 8003 / 24 / 22 bytes with FLUSH | 8070 bytes | 300071 bytes |
| q4, q5, q11 | about 8003 / 13 / 12 bytes with FLUSH | — | 300027 bytes (q5) |

q0/q1 compress each input fragment independently with a freshly cleared hash
table and a bounded fragment size. q2 and above keep a persistent hasher over
the ring buffer.

**Expected.** That `lgwin` and "matched windows" bound match reach for every
quality.

**Impact.** Under per-read streaming, q1 cannot reference earlier reads at all,
which inflates the q1→q11 gap and makes q1 a poor cold path. D-016 makes the
fallback q2-class and requires reporting each quality's effective reach.
Baseline tools must feed inputs with the same call and flush pattern they claim.
Checked in M1 (`chunky reach`): rust-brotli `6518a4e` and C v1.2.1 behave the
same, and a single q0/q1 call reaches back 64 KiB but not 256 KiB.

## RE-001: Partial literal output does not require a completed copy  (2026-10-21, status: worked-around)

**Environment.** Local `libbrotlidec.so.1`, `BrotliDecoderVersion() = 0x1001000`.
Rechecked during the design revision with the streaming C API via Python ctypes.

**Repro.** The complete RFC 7932 stream, in hex, is
`70000000549858c052504475`. It declares an 8-byte meta-block and one command
with insert length 6, copy length 2 and distance 2; literals are `ababab`.
Feed the first 11 bytes to `BrotliDecoderDecompressStream` with sufficient output
space: status is `NEEDS_MORE_INPUT` and output is `aba`. Feed the final byte:
status is `SUCCESS` and output is `babab`, giving exactly `abababab` in total.
The distance begins at bit 91, after the first 88 supplied bits, so the initial
output precedes completion of both the literal run and the copy distance.

**Observed vs expected.** D-010 (since culled) incorrectly inferred that every mid-meta-block
input pause must complete a copy. The decoder can expose literals mid-command;
the command's copy-length field appearing first does not prevent that output.
The [reference decoder](https://github.com/google/brotli/blob/master/c/dec/decode.c)
handles pending literal/distance states and pushes available output when input
runs out. D-012 corrects the invariant.

**Impact.** Measure pending fields, unmatched input tails and alignment together;
a tail-copy split does not establish a universal 2–4-byte delay bound. This
probe establishes C-decoder behavior only. Browser parser delivery and the
Worker edge still need independent timing measurements. Initial streaming uses
actual-length per-flush meta-blocks without decoded padding.
