"""Extract the HTTP Archive multi-origin sample (D-026, M1) into data/multi-origin/.

Two slices from the 2026-10-01 crawl, `client = 'mobile'`, root pages,
`type IN ('html','script','css')`, reported separately:

- `top10k`: `rank <= 10000` (CrUX rank buckets 1,000, 5,000 and 10,000).
- `random`: `TABLESAMPLE SYSTEM (0.03 PERCENT)` with no rank filter
  (block-level sample; the sample differs on every run, so run it once).

Each slice is one query that scans the source once and writes a scratch table
with the needed columns, the body SHA-256 computed in BigQuery, and the body
itself only on the first row per distinct body (dedup before egress). The
table is exported as gzipped NDJSON to a scratch bucket, downloaded, and
ingested into `manifest.jsonl`, `bodies/<sha256>` and `summary.json`.

Cost discipline (D-026): every query is dry-run first, every real query runs
with `--maximum_bytes_billed=50000000000` (50 GB), and the result is written
locally once. Log of dry-run/billed bytes: `<slice dir>/bq_log.jsonl`.

Reproduce (from the repo root; GCP project `httparchive`):

    D=data/multi-origin/httparchive-2026-10-01
    B=gs://httparchive-chunky-scratch-<random>      # pick a fresh name

    # Scratch resources (both short-lived; deleted at the end)
    bq mk --dataset --location=US --default_table_expiration=172800 \
        httparchive:chunky_scratch
    gcloud storage buckets create $B --project=httparchive --location=US \
        --uniform-bucket-level-access
    echo '{"rule":[{"action":{"type":"Delete"},"condition":{"age":2}}]}' > /tmp/lc.json
    gcloud storage buckets update $B --lifecycle-file=/tmp/lc.json

    for S in top10k random; do
      python3 -I tools/corpus/httparchive.py sql      --slice $S   # print SQL
      python3 -I tools/corpus/httparchive.py dryrun   --slice $S
      python3 -I tools/corpus/httparchive.py run      --slice $S   # 50 GB cap
      python3 -I tools/corpus/httparchive.py export   --slice $S --bucket $B
      python3 -I tools/corpus/httparchive.py download --slice $S --bucket $B
      hostlock shared --label "chunky: ingest $S" -- \
        python3 -I tools/corpus/httparchive.py ingest --slice $S
    done

    # Cleanup
    gcloud storage rm -r $B
    bq rm -r -f -d httparchive:chunky_scratch

`ingest` can be re-run from `raw/` without touching BigQuery; `reflag`
recomputes the per-row `fidelity` class and `summary.json` from the manifest
and bodies alone. The downloaded `raw/*.json.gz` shards are kept (about
11 GB for both slices); delete them by hand if space matters.

Manifest `fidelity` (see FIDELITY_NOTE): HTTP Archive's `response_body` is
not always the served bytes. `payload._body_hash` (sha256 of the body as the
crawler received it) disagrees with sha256(response_body) for 5.1% of
top10k body rows and 7.7% of random body rows: CRLF stored as LF, a stripped
BOM, or truncation (including an approximately 8 MB storage cap). Filter on
`fidelity in ("exact", "unverified")` when exact bytes matter.
"""

from __future__ import annotations

import argparse
import glob
import gzip
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpuslib  # noqa: E402

CRAWL_DATE = "2026-10-01"
PROJECT = "httparchive"
SCRATCH_DATASET = "chunky_scratch"
MAX_BYTES_BILLED = 50_000_000_000
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT_ROOT = os.path.join(REPO, "data", "multi-origin", "httparchive-" + CRAWL_DATE)

SLICES = {
    "top10k": {"sample": "", "extra_where": "AND rank <= 10000"},
    "random": {"sample": "TABLESAMPLE SYSTEM (0.03 PERCENT)", "extra_where": ""},
}

TYPE_MAP = {"html": "html", "script": "js", "css": "css"}


def sql_for(slice_name: str) -> str:
    s = SLICES[slice_name]

    def hdr(name: str) -> str:
        return (
            f"(SELECT value FROM UNNEST(response_headers) "
            f"WHERE LOWER(name) = '{name}' LIMIT 1)"
        )

    return f"""
WITH r AS (
  SELECT
    page, rank, url, type, is_main_document,
    {hdr('content-type')} AS h_content_type,
    {hdr('content-encoding')} AS h_content_encoding,
    {hdr('content-length')} AS h_content_length,
    {hdr('cache-control')} AS h_cache_control,
    {hdr('vary')} AS h_vary,
    EXISTS(SELECT 1 FROM UNNEST(response_headers)
           WHERE LOWER(name) = 'set-cookie') AS h_set_cookie,
    JSON_VALUE(payload, '$._responseCode') AS p_status,
    JSON_VALUE(payload, '$._contentEncoding') AS p_content_encoding,
    JSON_VALUE(payload, '$._contentType') AS p_content_type,
    JSON_VALUE(payload, '$._objectSize') AS p_object_size,
    JSON_VALUE(payload, '$._objectSizeUncompressed') AS p_object_size_uncompressed,
    JSON_VALUE(payload, '$._bytesIn') AS p_bytes_in,
    JSON_VALUE(payload, '$._body_hash') AS p_body_hash,
    IF(type = 'html', TO_JSON_STRING(JSON_QUERY(payload, '$._chunks')), NULL) AS p_chunks,
    response_body
  FROM `httparchive.crawl.requests` {s['sample']}
  WHERE date = '{CRAWL_DATE}'
    AND client = 'mobile'
    AND is_root_page
    AND type IN ('html', 'script', 'css')
    {s['extra_where']}
)
SELECT
  * EXCEPT (response_body),
  response_body IS NULL AS body_is_null,
  BYTE_LENGTH(response_body) AS body_bytes,
  TO_HEX(SHA256(response_body)) AS body_sha256,
  IF(ROW_NUMBER() OVER (PARTITION BY TO_HEX(SHA256(response_body))
                        ORDER BY rank, url, page) = 1,
     response_body, NULL) AS body
FROM r
""".strip()


def slice_dir(slice_name: str) -> str:
    return os.path.join(OUT_ROOT, slice_name)


def table_for(slice_name: str) -> str:
    return f"{PROJECT}:{SCRATCH_DATASET}.{slice_name}_{CRAWL_DATE.replace('-', '')}"


def log(slice_name: str, rec: dict) -> None:
    d = slice_dir(slice_name)
    os.makedirs(d, exist_ok=True)
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **rec}
    with open(os.path.join(d, "bq_log.jsonl"), "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps(rec))


def run(cmd: list[str], capture: bool = True) -> str:
    print("+", " ".join(cmd[:6]), "..." if len(cmd) > 6 else "", file=sys.stderr)
    p = subprocess.run(cmd, capture_output=capture, text=True)
    if p.returncode != 0:
        sys.stderr.write((p.stdout or "") + (p.stderr or ""))
        raise SystemExit(f"command failed: {cmd[0]} rc={p.returncode}")
    return p.stdout if capture else ""


def cmd_dryrun(a) -> None:
    out = run(
        [
            "bq", "--project_id", PROJECT, "query", "--dry_run", "--use_legacy_sql=false",
            "--format=json", sql_for(a.slice),
        ]
    )
    d = json.loads(out)
    b = int(d["statistics"]["totalBytesProcessed"])
    log(a.slice, {"op": "dryrun", "bytes_processed": b, "gb": round(b / 1e9, 3)})


def cmd_run(a) -> None:
    job_id = f"chunky_{a.slice}_{CRAWL_DATE.replace('-', '')}_{int(time.time())}"
    run(
        [
            "bq", "--quiet", "--project_id", PROJECT, "query", "--use_legacy_sql=false",
            f"--maximum_bytes_billed={MAX_BYTES_BILLED}",
            f"--destination_table={table_for(a.slice)}",
            "--replace", f"--job_id={job_id}", "--format=none",
            sql_for(a.slice),
        ],
        capture=False,
    )
    j = json.loads(run(["bq", "--project_id", PROJECT, "show", "--format=json", "-j", job_id]))
    st = j["statistics"]
    q = st.get("query", {})
    tbl = json.loads(run(["bq", "show", "--format=json", table_for(a.slice)]))
    log(
        a.slice,
        {
            "op": "run",
            "job_id": job_id,
            "bytes_processed": int(q.get("totalBytesProcessed", 0)),
            "bytes_billed": int(q.get("totalBytesBilled", 0)),
            "gb_billed": round(int(q.get("totalBytesBilled", 0)) / 1e9, 3),
            "slot_ms": int(q.get("totalSlotMs", 0)),
            "dest_table": table_for(a.slice),
            "dest_rows": int(tbl.get("numRows", 0)),
            "dest_bytes": int(tbl.get("numBytes", 0)),
        },
    )


def cmd_export(a) -> None:
    uri = f"{a.bucket.rstrip('/')}/{a.slice}/part-*.json.gz"
    run(
        [
            "bq", "--project_id", PROJECT, "extract",
            "--destination_format=NEWLINE_DELIMITED_JSON", "--compression=GZIP",
            table_for(a.slice), uri,
        ],
        capture=False,
    )
    log(a.slice, {"op": "export", "uri": uri})


def cmd_download(a) -> None:
    raw = os.path.join(slice_dir(a.slice), "raw")
    os.makedirs(raw, exist_ok=True)
    src = f"{a.bucket.rstrip('/')}/{a.slice}/*"
    run(["gcloud", "storage", "cp", src, raw + "/"], capture=False)
    total = sum(os.path.getsize(p) for p in glob.glob(os.path.join(raw, "*.json.gz")))
    log(a.slice, {"op": "download", "src": src, "files": len(os.listdir(raw)), "bytes": total})


def _int(v):
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def cmd_ingest(a) -> None:
    d = slice_dir(a.slice)
    raw_files = sorted(glob.glob(os.path.join(d, "raw", "*.json.gz")))
    if not raw_files:
        raise SystemExit("no raw/*.json.gz; run download first")
    store = corpuslib.BodyStore(d)
    rows = []
    mismatch = 0
    nbody_rows = 0
    for path in raw_files:
        with gzip.open(path, "rt", encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                body = r.get("body")
                sha = r.get("body_sha256")
                sha = sha.lower() if sha else None
                if body is not None:
                    nbody_rows += 1
                    got = store.put(body.encode("utf-8"))
                    if got != sha:
                        mismatch += 1
                        print("sha mismatch", r["url"], got, sha, file=sys.stderr)
                url = r["url"]
                page = r["page"]
                host = corpuslib.host_of(url)
                domain = corpuslib.registrable_domain(host)
                page_domain = corpuslib.registrable_domain(corpuslib.host_of(page))
                nbytes = _int(r.get("body_bytes")) or 0
                status = _int(r.get("p_status"))
                cc = (r.get("h_cache_control") or "").lower()
                enc = (r.get("p_content_encoding") or r.get("h_content_encoding") or "").strip().lower()
                osu = _int(r.get("p_object_size_uncompressed"))
                flags = []
                if r.get("body_is_null") in (True, "true"):
                    flags.append("null_body")
                    sha = None
                elif nbytes == 0:
                    flags.append("empty")
                if domain != page_domain:
                    flags.append("third_party")
                if r.get("h_set_cookie") in (True, "true"):
                    flags.append("set_cookie")
                if "private" in cc:
                    flags.append("cc_private")
                if "no-store" in cc:
                    flags.append("cc_no_store")
                if status != 200:
                    flags.append("status_%s" % status)
                pbh = (r.get("p_body_hash") or "").lower()
                if sha and pbh and pbh != sha:
                    flags.append("ha_body_hash_differs")
                if sha and osu is not None and nbytes and osu != nbytes:
                    flags.append("size_differs_uncompressed" if nbytes < osu else "size_exceeds_uncompressed")
                t = TYPE_MAP.get(r["type"], r["type"])
                row = {
                    "source": "httparchive",
                    "slice": a.slice,
                    "crawl": CRAWL_DATE,
                    "client": "mobile",
                    "url": url,
                    "page": page,
                    "host": host,
                    "domain": domain,
                    "page_domain": page_domain,
                    "rank": _int(r.get("rank")),
                    "type": t,
                    "is_main_document": r.get("is_main_document") in (True, "true"),
                    "status": status,
                    "content_type": r.get("h_content_type") or r.get("p_content_type"),
                    "content_encoding": enc or None,
                    "content_length": _int(r.get("h_content_length")),
                    "cache_control": r.get("h_cache_control"),
                    "vary": r.get("h_vary"),
                    "transfer_bytes": _int(r.get("p_object_size")),
                    "uncompressed_bytes_reported": osu,
                    "bytes": nbytes,
                    "body_sha256": sha,
                    "ha_body_hash": pbh or None,
                    "split": corpuslib.split_for_domain(domain),
                    "flags": flags,
                }
                if r.get("p_chunks") and t == "html":
                    try:
                        ch = json.loads(r["p_chunks"])
                        row["net_chunks"] = [[c.get("ts"), c.get("bytes")] for c in ch or []]
                    except (ValueError, AttributeError, TypeError):
                        pass
                rows.append(row)
    missing = [x for x in rows if x["body_sha256"] and not os.path.exists(os.path.join(store.dir, x["body_sha256"]))]
    for x in missing:
        x["flags"].append("body_missing_locally")
    bq = []
    lp = os.path.join(d, "bq_log.jsonl")
    if os.path.exists(lp):
        with open(lp) as f:
            bq = [json.loads(line) for line in f]
    extra = {
        "source": "httparchive",
        "slice": a.slice,
        "crawl": CRAWL_DATE,
        "query": sql_for(a.slice),
        "bq_log": bq,
        "rank_note": "HTTP Archive rank is a CrUX rank bucket (1000, 5000, 10000, 50000, ...), not an exact rank",
        "body_rows_downloaded": nbody_rows,
        "sha_mismatch_bq_vs_local": mismatch,
        "rows_body_missing_locally": len(missing),
    }
    finalize(d, rows, extra)


# Fidelity of the stored body against what the crawler saw. HTTP Archive's
# payload._body_hash is sha256 of the decoded response body as received;
# when it differs from sha256(response_body) the BigQuery copy is not the
# original bytes. Classified from the size reported in
# payload._objectSizeUncompressed (osu) and the stored bytes.
FIDELITY_NOTE = {
    "exact": "sha256(body) == payload._body_hash",
    "unverified": "no payload._body_hash; size agrees with osu or osu unknown",
    "unverified_shorter": "no payload._body_hash; body shorter than osu",
    "crlf_normalized": "body has no CR and osu - len == count(LF) (+0..3 for a BOM): CRLF stored as LF",
    "bom_stripped": "osu - len == 3",
    "truncated_cap": "len >= 7.9 MB and shorter than osu: storage size cap",
    "shorter_unexplained": "shorter than osu otherwise; sampled tails end mid-token (truncated)",
    "longer": "longer than osu (charset transcoding to UTF-8 or replacement characters)",
    "same_size_differs": "same size as osu but different bytes (transcoding or replacement)",
    "none": "no body",
}


def classify_fidelity(store_dir: str, r: dict, cache: dict) -> str:
    sha = r["body_sha256"]
    if not sha:
        return "none"
    osu = r.get("uncompressed_bytes_reported")
    n = r["bytes"]
    if r.get("ha_body_hash"):
        if r["ha_body_hash"] == sha:
            return "exact"
    else:
        return "unverified_shorter" if osu is not None and n < osu else "unverified"
    key = (sha, osu)
    if key in cache:
        return cache[key]
    if osu is None or osu == n:
        c = "same_size_differs"
    elif n > osu:
        c = "longer"
    else:
        diff = osu - n
        with open(os.path.join(store_dir, sha), "rb") as f:
            b = f.read()
        nl = b.count(b"\n")
        if b"\r" not in b and nl and 0 <= diff - nl <= 3:
            c = "crlf_normalized"
        elif diff == 3:
            c = "bom_stripped"
        elif n >= 7_900_000:
            c = "truncated_cap"
        else:
            c = "shorter_unexplained"
    cache[key] = c
    return c


def finalize(d: str, rows: list[dict], extra: dict) -> None:
    from collections import Counter

    store_dir = os.path.join(d, "bodies")
    cache: dict = {}
    fid = Counter()
    fid_type: dict[str, Counter] = {}
    null_status = Counter()
    for x in rows:
        x["fidelity"] = classify_fidelity(store_dir, x, cache)
        fid[x["fidelity"]] += 1
        fid_type.setdefault(x["type"], Counter())[x["fidelity"]] += 1
        if "null_body" in x["flags"]:
            null_status[str(x["status"])] += 1
    rows.sort(key=lambda x: (x["rank"] or 0, x["page"], x["url"]))
    with open(os.path.join(d, "manifest.jsonl"), "w") as f:
        for x in rows:
            f.write(json.dumps(x, separators=(",", ":")) + "\n")
    extra = {
        **extra,
        "fidelity": dict(fid.most_common()),
        "fidelity_by_type": {t: dict(c.most_common()) for t, c in sorted(fid_type.items())},
        "fidelity_rules": FIDELITY_NOTE,
        "null_body_by_status": dict(null_status.most_common()),
        "main_documents": sum(1 for x in rows if x["is_main_document"]),
        "main_documents_with_body": sum(1 for x in rows if x["is_main_document"] and x["bytes"] > 0),
    }
    s = corpuslib.write_summary(d, rows, extra)
    print(json.dumps({k: s[k] for k in ("rows", "bytes", "distinct_bodies", "distinct_body_bytes", "distinct_domains", "fidelity")}))


def cmd_reflag(a) -> None:
    """Recompute fidelity and summary from manifest.jsonl and bodies/ (no raw/ needed)."""
    d = slice_dir(a.slice)
    with open(os.path.join(d, "manifest.jsonl")) as f:
        rows = [json.loads(line) for line in f]
    with open(os.path.join(d, "summary.json")) as f:
        old = json.load(f)
    keep = ("source", "slice", "crawl", "query", "bq_log", "rank_note", "body_rows_downloaded",
            "sha_mismatch_bq_vs_local", "rows_body_missing_locally")
    finalize(d, rows, {k: old[k] for k in keep if k in old})


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("sql", "dryrun", "run", "export", "download", "ingest", "reflag"):
        p = sub.add_parser(name)
        p.add_argument("--slice", choices=sorted(SLICES), required=True)
        if name in ("export", "download"):
            p.add_argument("--bucket", required=True)
    a = ap.parse_args()
    if a.cmd == "sql":
        print(sql_for(a.slice))
    else:
        {"dryrun": cmd_dryrun, "run": cmd_run, "export": cmd_export,
         "download": cmd_download, "ingest": cmd_ingest, "reflag": cmd_reflag}[a.cmd](a)


if __name__ == "__main__":
    main()
