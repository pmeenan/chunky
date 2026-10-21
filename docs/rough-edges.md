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
"something weird".

---

*(no findings yet)*
