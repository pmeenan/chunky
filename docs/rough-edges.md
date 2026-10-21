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
