"""Sample Common Crawl HTML for the multi-origin and per-tenant corpus (D-022, D-026, M1).

Sources (D-022 "Second and third sources"), html with status 200:

- `wikipedia-en`   en.wikipedia.org/wiki/          SURT `org,wikipedia,en)/wiki/`
- `commons`        commons.wikimedia.org/wiki/     SURT `org,wikimedia,commons)/wiki/`
- `wiktionary-en`  en.wiktionary.org/wiki/         SURT `org,wiktionary,en)/wiki/`
- `shopify-<store>` one tenant per `<store>.myshopify.com` storefront with
  >= 30 crawled html pages (discovered with `shopify-discover`).

Access follows RE-008: no CDX server, no Parquet scans. `cluster.idx` is
downloaded once per snapshot (about 105 MB), the needed CDX blocks are
range-fetched and cached under `data/multi-origin/commoncrawl/_index/`, and
WARC records are range-fetched one at a time with a pause between requests.
Any 403/429/503 or timeout backs off (30 s, 60 s, 120 s) and three failures in
a row stop the run; re-running resumes (URLs already in a manifest are
skipped).

URL selection is deterministic: a source picks `--blocks` evenly spaced CDX
blocks from its SURT range in the primary snapshot, keeps html/200 records,
dedups by URL and takes the `--n` URLs with the smallest sha256(url). The same
URLs are then looked up in each `--also` snapshot (chronological slices,
natural template changes). Common Crawl rarely recaptures the same page
month to month, so when fewer than half are found, an independent sample is
added from the other snapshot by the same rule. A Shopify store reads all of
its CDX blocks, so it takes up to `--n` of its html/200 URLs per snapshot.

Layout per source: `data/multi-origin/commoncrawl/<source>/manifest.jsonl`,
`bodies/<sha256>`, `summary.json`. Bodies are the WARC response payloads with
any transfer/content coding removed (Common Crawl stores them decoded and
renames the served header to `X-Crawler-Content-Encoding`). Records with
`WARC-Truncated` are kept and flagged `truncated`.

Split: the same domain-hash rule as HTTP Archive (corpuslib.split_for_domain)
on the registrable domain, so each Wikimedia project and each Shopify store
(`<store>.myshopify.com` is a PSL private suffix entry) lands wholly in one
split. Shopify rows also carry `tenant = shopify-<store>` (D-022: no
cross-store learning).

Reproduce (from the repo root; network only, no hostlock needed):

    P="python3 -I tools/corpus/commoncrawl.py"
    $P index --crawl CC-MAIN-2026-39
    $P index --crawl CC-MAIN-2026-43
    for S in wikipedia-en commons wiktionary-en; do
      $P sample --source $S --crawl CC-MAIN-2026-39 --also CC-MAIN-2026-43 \
         --blocks 3 --n 150
    done
    $P shopify-discover --crawl CC-MAIN-2026-39 --blocks 12   # prints stores
    # 2026-10-22 run used these five (>= 134 html/200 pages each in -39):
    for ST in dijkmansmuziek bohomegirls magnolia-pearl-clothing \
              mij-factory-harajuku joya-socks; do
      $P sample --source shopify-$ST --crawl CC-MAIN-2026-39 \
         --also CC-MAIN-2026-43 --n 60
    done
    $P summarize --source all
"""

from __future__ import annotations

import argparse
import bisect
import gzip
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
import zlib
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpuslib  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT_ROOT = os.path.join(REPO, "data", "multi-origin", "commoncrawl")
IDX_ROOT = os.path.join(OUT_ROOT, "_index")
DATA_URL = "https://data.commoncrawl.org/"
UA = "chunky-corpus/0.1 (research sample; sequential, paced)"
PAUSE_S = 1.0

SOURCES = {
    "wikipedia-en": "org,wikipedia,en)/wiki/",
    "commons": "org,wikimedia,commons)/wiki/",
    "wiktionary-en": "org,wiktionary,en)/wiki/",
}
SHOPIFY_PREFIX = "com,myshopify,"


class Throttled(SystemExit):
    pass


class Fetcher:
    def __init__(self):
        self.fails = 0
        self.requests = 0
        self.bytes = 0
        self.last = 0.0

    def get(self, url: str, rng: tuple[int, int] | None = None, timeout: int = 60) -> bytes:
        while True:
            wait = self.last + PAUSE_S - time.time()
            if wait > 0:
                time.sleep(wait)
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            if rng:
                req.add_header("Range", f"bytes={rng[0]}-{rng[0] + rng[1] - 1}")
            self.last = time.time()
            self.requests += 1
            try:
                with urllib.request.urlopen(req, timeout=timeout) as r:
                    data = r.read()
                self.fails = 0
                self.bytes += len(data)
                return data
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError) as e:
                code = getattr(e, "code", None)
                if code is not None and code not in (403, 429, 500, 502, 503, 504):
                    raise
                self.fails += 1
                print(f"fetch failed ({e}); fails={self.fails}", file=sys.stderr)
                if self.fails >= 3:
                    raise Throttled(f"stopping after 3 consecutive failures: {e}")
                time.sleep(30 * (2 ** (self.fails - 1)))


FETCH = Fetcher()


# ---------------------------------------------------------------- index


def idx_dir(crawl: str) -> str:
    return os.path.join(IDX_ROOT, crawl)


def cmd_index(a) -> None:
    d = idx_dir(a.crawl)
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, "cluster.idx")
    if os.path.exists(p):
        print("exists", p)
        return
    url = f"{DATA_URL}cc-index/collections/{a.crawl}/indexes/cluster.idx"
    data = FETCH.get(url, timeout=600)
    with open(p + ".tmp", "wb") as f:
        f.write(data)
    os.replace(p + ".tmp", p)
    print("wrote", p, len(data))


class ClusterIdx:
    def __init__(self, crawl: str):
        self.crawl = crawl
        self.keys: list[str] = []
        self.blocks: list[tuple[str, int, int]] = []
        with open(os.path.join(idx_dir(crawl), "cluster.idx"), encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                self.keys.append(parts[0])
                self.blocks.append((parts[1], int(parts[2]), int(parts[3])))

    def range_for_prefix(self, prefix: str) -> range:
        lo = max(0, bisect.bisect_left(self.keys, prefix) - 1)
        hi = bisect.bisect_left(self.keys, prefix + "￿")
        return range(lo, hi)

    def block_for_key(self, key: str) -> int:
        return max(0, bisect.bisect_right(self.keys, key) - 1)

    def read_block(self, i: int) -> list[tuple[str, str, dict]]:
        fn, off, ln = self.blocks[i]
        cache = os.path.join(idx_dir(self.crawl), "blocks", f"{fn}_{off}.gz")
        if not os.path.exists(cache):
            os.makedirs(os.path.dirname(cache), exist_ok=True)
            url = f"{DATA_URL}cc-index/collections/{self.crawl}/indexes/{fn}"
            data = FETCH.get(url, (off, ln))
            with open(cache + ".tmp", "wb") as f:
                f.write(data)
            os.replace(cache + ".tmp", cache)
        out = []
        with gzip.open(cache, "rt", encoding="utf-8") as f:
            for line in f:
                surt, ts, js = line.rstrip("\n").split(" ", 2)
                out.append((surt, ts, json.loads(js)))
        return out


def is_html_200(rec: dict) -> bool:
    if rec.get("status") != "200":
        return False
    m = (rec.get("mime-detected") or rec.get("mime") or "").lower()
    return m in ("text/html", "application/xhtml+xml")


# ---------------------------------------------------------------- WARC


def parse_warc(data: bytes) -> dict:
    raw = gzip.decompress(data)
    wh_end = raw.index(b"\r\n\r\n")
    wh = raw[:wh_end].decode("utf-8", "replace").split("\r\n")
    warc = {}
    for line in wh[1:]:
        k, _, v = line.partition(":")
        warc[k.strip().lower()] = v.strip()
    rest = raw[wh_end + 4 :]
    clen = int(warc.get("content-length", len(rest)))
    rest = rest[:clen]
    hh_end = rest.index(b"\r\n\r\n")
    hlines = rest[:hh_end].decode("iso-8859-1").split("\r\n")
    status_line = hlines[0]
    headers: list[tuple[str, str]] = []
    for line in hlines[1:]:
        k, _, v = line.partition(":")
        headers.append((k.strip().lower(), v.strip()))
    body = rest[hh_end + 4 :]
    return {"warc": warc, "status_line": status_line, "headers": headers, "body": body}


def dechunk(b: bytes) -> bytes:
    out = bytearray()
    i = 0
    while True:
        j = b.index(b"\r\n", i)
        n = int(b[i:j].split(b";")[0], 16)
        if n == 0:
            return bytes(out)
        out += b[j + 2 : j + 2 + n]
        i = j + 2 + n + 2


def hget(headers, name):
    for k, v in headers:
        if k == name:
            return v
    return None


# ---------------------------------------------------------------- sampling


def source_dir(source: str) -> str:
    return os.path.join(OUT_ROOT, source)


def load_manifest(d: str) -> list[dict]:
    p = os.path.join(d, "manifest.jsonl")
    if not os.path.exists(p):
        return []
    with open(p) as f:
        return [json.loads(line) for line in f]


def fetch_and_store(source: str, crawl: str, surt: str, ts: str, rec: dict, store, mf) -> dict:
    data = FETCH.get(DATA_URL + rec["filename"], (int(rec["offset"]), int(rec["length"])))
    w = parse_warc(data)
    flags = []
    body = w["body"]
    te = (hget(w["headers"], "transfer-encoding") or "").lower()
    if "chunked" in te:
        try:
            body = dechunk(body)
            flags.append("dechunked")
        except (ValueError, IndexError):
            flags.append("dechunk_failed")
    ce = (hget(w["headers"], "content-encoding") or "").lower()
    if ce in ("gzip", "x-gzip", "deflate"):
        try:
            body = zlib.decompress(body, 47 if "gzip" in ce else 15)
            flags.append("decoded_" + ce)
        except zlib.error:
            flags.append("still_encoded_" + ce)
    elif ce and ce != "identity":
        flags.append("still_encoded_" + ce)
    if w["warc"].get("warc-truncated"):
        flags.append("truncated")
    sha = store.put(body)
    url = rec["url"]
    host = corpuslib.host_of(url)
    domain = corpuslib.registrable_domain(host)
    tenant = source if source.startswith("shopify-") else domain
    cc = (hget(w["headers"], "cache-control") or "").lower()
    if hget(w["headers"], "set-cookie"):
        flags.append("set_cookie")
    if "private" in cc:
        flags.append("cc_private")
    if "no-store" in cc:
        flags.append("cc_no_store")
    if not body:
        flags.append("empty")
    row = {
        "source": "commoncrawl",
        "slice": source,
        "crawl": crawl,
        "timestamp": ts,
        "url": url,
        "surt": surt,
        "page": url,
        "host": host,
        "domain": domain,
        "tenant": tenant,
        "rank": None,
        "type": "html",
        "status": int(rec.get("status", 0)),
        "content_type": hget(w["headers"], "content-type"),
        "content_encoding": (
            hget(w["headers"], "x-crawler-content-encoding") or hget(w["headers"], "content-encoding")
        ),
        "cache_control": hget(w["headers"], "cache-control"),
        "vary": hget(w["headers"], "vary"),
        "charset_detected": rec.get("encoding"),
        "languages": rec.get("languages"),
        "warc_filename": rec["filename"],
        "warc_offset": int(rec["offset"]),
        "warc_length": int(rec["length"]),
        "warc_digest": rec.get("digest"),
        "bytes": len(body),
        "body_sha256": sha,
        "split": corpuslib.split_for_domain(domain),
        "flags": flags,
    }
    mf.write(json.dumps(row, separators=(",", ":")) + "\n")
    mf.flush()
    return row


def collect(idx: ClusterIdx, blocks, prefix: str) -> dict[str, tuple[str, str, dict]]:
    """url -> (surt, ts, rec) for html/200 records under prefix (first capture wins)."""
    out: dict[str, tuple[str, str, dict]] = {}
    for i in blocks:
        for surt, ts, rec in idx.read_block(i):
            if surt.startswith(prefix) and is_html_200(rec) and rec["url"] not in out:
                out[rec["url"]] = (surt, ts, rec)
    return out


def cmd_sample(a) -> None:
    src = a.source
    if src in SOURCES:
        prefix = SOURCES[src]
    elif src.startswith("shopify-"):
        prefix = f"{SHOPIFY_PREFIX}{src[len('shopify-'):]})/"
    else:
        raise SystemExit(f"unknown source {src}")
    d = source_dir(src)
    os.makedirs(d, exist_ok=True)
    have = {(r["crawl"], r["url"]) for r in load_manifest(d)}
    store = corpuslib.BodyStore(d)
    idx = ClusterIdx(a.crawl)
    rng = idx.range_for_prefix(prefix)
    print(f"{src}: {len(rng)} CDX blocks in {a.crawl} for {prefix}", file=sys.stderr)
    if src.startswith("shopify-") or a.blocks <= 0 or a.blocks >= len(rng):
        blocks = list(rng)
    else:
        step = len(rng) / a.blocks
        blocks = sorted({rng[int(step * (k + 0.5))] for k in range(a.blocks)})
    recs = collect(idx, blocks, prefix)
    urls = sorted(recs, key=lambda u: hashlib.sha256(u.encode()).digest())[: a.n]
    urls.sort(key=lambda u: recs[u][0])
    print(f"{src}: {len(recs)} html/200 urls in {len(blocks)} blocks; taking {len(urls)}", file=sys.stderr)
    plan = [(a.crawl, idx, {u: recs[u] for u in urls})]
    for other in a.also or []:
        oidx = ClusterIdx(other)
        need = sorted({oidx.block_for_key(recs[u][0]) for u in urls})
        orecs = collect(oidx, need, prefix)
        found = {u: orecs[u] for u in urls if u in orecs}
        print(f"{src}: {other}: {len(found)}/{len(urls)} same urls (html/200) in {len(need)} blocks", file=sys.stderr)
        if len(found) < len(urls) // 2:
            # Common Crawl rarely recaptures the same article month to month,
            # so add an independent sample by the same rule from `other`.
            orng = oidx.range_for_prefix(prefix)
            if a.blocks <= 0 or a.blocks >= len(orng):
                oblocks = list(orng)
            else:
                ostep = len(orng) / a.blocks
                oblocks = sorted({orng[int(ostep * (k + 0.5))] for k in range(a.blocks)})
            extra = collect(oidx, oblocks, prefix)
            pick = sorted(extra, key=lambda u: hashlib.sha256(u.encode()).digest())[: a.n - len(found)]
            for u in sorted(pick, key=lambda u: extra[u][0]):
                found.setdefault(u, extra[u])
            print(f"{src}: {other}: +{len(pick)} independent urls from {len(oblocks)} blocks", file=sys.stderr)
        plan.append((other, oidx, found))
    with open(os.path.join(d, "manifest.jsonl"), "a") as mf:
        try:
            for crawl, _, sel in plan:
                n = 0
                for u, (surt, ts, rec) in sel.items():
                    if (crawl, u) in have:
                        continue
                    fetch_and_store(src, crawl, surt, ts, rec, store, mf)
                    have.add((crawl, u))
                    n += 1
                print(f"{src}: {crawl}: fetched {n}", file=sys.stderr)
        finally:
            print(f"requests={FETCH.requests} bytes={FETCH.bytes}", file=sys.stderr)
    summarize(src)


def cmd_shopify_discover(a) -> None:
    idx = ClusterIdx(a.crawl)
    rng = idx.range_for_prefix(SHOPIFY_PREFIX)
    step = max(1, len(rng) / max(1, a.blocks))
    blocks = sorted({rng[int(step * (k + 0.5))] for k in range(min(a.blocks, len(rng)))})
    print(f"{len(rng)} CDX blocks under {SHOPIFY_PREFIX}; reading {len(blocks)}", file=sys.stderr)
    per = Counter()
    edge = set()
    for i in blocks:
        # Boundary blocks also hold records just outside the prefix.
        rows = [row for row in idx.read_block(i) if row[0].startswith(SHOPIFY_PREFIX)]
        stores = [s.split(")", 1)[0][len(SHOPIFY_PREFIX):] for s, _, _ in rows]
        if stores:
            edge.update({stores[0], stores[-1]})
        seen = set()
        for (surt, _, rec), st in zip(rows, stores):
            if is_html_200(rec) and rec["url"] not in seen:
                seen.add(rec["url"])
                per[st] += 1
    for st, n in per.most_common(40):
        if n >= a.min_pages:
            print(f"{st}\t{n}\t{'(block edge: count is a lower bound)' if st in edge else ''}")


def summarize(source: str) -> None:
    d = source_dir(source)
    rows = load_manifest(d)
    if not rows:
        return
    by_crawl = Counter(r["crawl"] for r in rows)
    urls_by_crawl: dict[str, set] = {}
    for r in rows:
        urls_by_crawl.setdefault(r["crawl"], set()).add(r["url"])
    crawls = sorted(urls_by_crawl)
    common = set.intersection(*urls_by_crawl.values()) if len(crawls) > 1 else set()
    corpuslib.write_summary(
        d,
        rows,
        {
            "source": "commoncrawl",
            "slice": source,
            "rows_by_crawl": dict(by_crawl),
            "urls_in_all_crawls": len(common),
            "identical_body_across_crawls": sum(
                1
                for u in common
                if len({r["body_sha256"] for r in rows if r["url"] == u}) == 1
            ),
        },
    )


def cmd_summarize(a) -> None:
    srcs = (
        [s for s in sorted(os.listdir(OUT_ROOT)) if not s.startswith("_")]
        if a.source == "all"
        else [a.source]
    )
    for s in srcs:
        summarize(s)
        print("summarized", s)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("index")
    p.add_argument("--crawl", required=True)
    p = sub.add_parser("sample")
    p.add_argument("--source", required=True)
    p.add_argument("--crawl", required=True)
    p.add_argument("--also", action="append")
    p.add_argument("--blocks", type=int, default=3)
    p.add_argument("--n", type=int, default=150)
    p = sub.add_parser("shopify-discover")
    p.add_argument("--crawl", required=True)
    p.add_argument("--blocks", type=int, default=12)
    p.add_argument("--min-pages", type=int, default=30)
    p = sub.add_parser("summarize")
    p.add_argument("--source", default="all")
    a = ap.parse_args()
    {
        "index": cmd_index,
        "sample": cmd_sample,
        "shopify-discover": cmd_shopify_discover,
        "summarize": cmd_summarize,
    }[a.cmd](a)


if __name__ == "__main__":
    main()
