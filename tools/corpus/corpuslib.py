"""Shared helpers for the multi-origin corpus tools (D-026, M1).

Imported by `httparchive.py` and `commoncrawl.py` in this directory. Python
stdlib only; run the tools with `python3 -I` (they add this directory to
`sys.path` themselves).

What lives here:

- `registrable_domain(host)`: eTLD+1 from the system Public Suffix List
  (`/usr/share/publicsuffix/public_suffix_list.dat`, Debian/Ubuntu package
  `publicsuffix`), falling back to the last two labels if the file is absent.
- `split_for_domain(domain)`: the held-out-origin split rule. A domain goes to
  train/tune/heldout = 60/20/20 by
  `int.from_bytes(sha256(domain.encode()).digest()[:8], "big") % 100`
  (<60 train, <80 tune, else heldout), so a domain's responses never span
  splits and the assignment is stable across slices and runs.
- `BodyStore`: content-addressed `bodies/<sha256>` writer (dedup by content).
- `write_summary(...)`: per-slice `summary.json` (counts and bytes by type,
  size percentiles, distinct domains, split stats).
"""

from __future__ import annotations

import hashlib
import json
import os
from collections import Counter, defaultdict
from urllib.parse import urlsplit

PSL_PATH = "/usr/share/publicsuffix/public_suffix_list.dat"

SPLIT_RULE = (
    "split = f(int.from_bytes(sha256(domain.encode('utf-8')).digest()[:8], 'big') % 100): "
    "<60 train, <80 tune, else heldout; domain = registrable domain (PSL eTLD+1) "
    "of the response URL host"
)


class _Psl:
    def __init__(self, path: str = PSL_PATH):
        self.rules: set[str] = set()
        self.wild: set[str] = set()
        self.exc: set[str] = set()
        self.available = os.path.exists(path)
        if not self.available:
            return
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("//"):
                    continue
                rule = line.split()[0].lower()
                if rule.startswith("!"):
                    self.exc.add(rule[1:])
                elif rule.startswith("*."):
                    self.wild.add(rule[2:])
                else:
                    self.rules.add(rule)

    def suffix_len(self, labels: list[str]) -> int:
        """Number of labels in the public suffix of `labels`."""
        n = len(labels)
        best = 1  # default rule "*"
        for i in range(n):
            cand = ".".join(labels[i:])
            k = n - i
            if cand in self.exc:
                return k - 1
            if cand in self.rules and k > best:
                best = k
            if i + 1 < n and ".".join(labels[i + 1 :]) in self.wild and k > best:
                best = k
        return best


_PSL: _Psl | None = None


def _psl() -> _Psl:
    global _PSL
    if _PSL is None:
        _PSL = _Psl()
    return _PSL


def psl_method() -> str:
    return "psl:" + PSL_PATH if _psl().available else "last-two-labels"


def host_of(url: str) -> str:
    try:
        h = urlsplit(url).hostname or ""
    except ValueError:
        h = ""
    return h.lower().rstrip(".")


def registrable_domain(host: str) -> str:
    host = host.lower().rstrip(".")
    if not host:
        return ""
    # IP literals stay as-is.
    if host.replace(".", "").isdigit() or ":" in host:
        return host
    labels = host.split(".")
    p = _psl()
    if p.available:
        k = p.suffix_len(labels)
    else:
        k = 1
    if k >= len(labels):
        return host
    return ".".join(labels[-(k + 1) :])


def split_for_domain(domain: str) -> str:
    h = int.from_bytes(hashlib.sha256(domain.encode("utf-8")).digest()[:8], "big") % 100
    if h < 60:
        return "train"
    if h < 80:
        return "tune"
    return "heldout"


class BodyStore:
    """Writes `bodies/<sha256>` once per distinct content."""

    def __init__(self, slice_dir: str):
        self.dir = os.path.join(slice_dir, "bodies")
        os.makedirs(self.dir, exist_ok=True)
        self.written = 0
        self.existing = 0

    def put(self, data: bytes) -> str:
        sha = hashlib.sha256(data).hexdigest()
        path = os.path.join(self.dir, sha)
        if os.path.exists(path):
            self.existing += 1
            return sha
        tmp = path + ".tmp"
        with open(tmp, "wb") as f:
            f.write(data)
        os.replace(tmp, path)
        self.written += 1
        return sha


def _pct(sorted_vals: list[int], q: float) -> int:
    if not sorted_vals:
        return 0
    i = min(len(sorted_vals) - 1, max(0, int(round(q * (len(sorted_vals) - 1)))))
    return sorted_vals[i]


def size_stats(vals: list[int]) -> dict:
    s = sorted(vals)
    return {
        "count": len(s),
        "bytes": sum(s),
        "min": s[0] if s else 0,
        "p10": _pct(s, 0.10),
        "p25": _pct(s, 0.25),
        "p50": _pct(s, 0.50),
        "p75": _pct(s, 0.75),
        "p90": _pct(s, 0.90),
        "p99": _pct(s, 0.99),
        "max": s[-1] if s else 0,
    }


SIZE_BUCKETS = [
    (0, "0"),
    (1024, "1-1K"),
    (8 * 1024, "1K-8K"),
    (64 * 1024, "8K-64K"),
    (512 * 1024, "64K-512K"),
    (1 << 62, "512K+"),
]


def size_bucket(n: int) -> str:
    if n == 0:
        return "0"
    for lim, name in SIZE_BUCKETS[1:]:
        if n <= lim:
            return name
    return SIZE_BUCKETS[-1][1]


def write_summary(slice_dir: str, rows: list[dict], extra: dict) -> dict:
    """Compute and write `summary.json` for one slice from its manifest rows."""
    by_type: dict[str, list[int]] = defaultdict(list)
    uniq_by_type: dict[str, dict[str, int]] = defaultdict(dict)
    domains_by_type: dict[str, set] = defaultdict(set)
    split_rows = Counter()
    split_bytes = Counter()
    split_domains: dict[str, set] = defaultdict(set)
    split_type_rows: dict[str, Counter] = defaultdict(Counter)
    buckets: dict[str, Counter] = defaultdict(Counter)
    flags = Counter()
    hash_splits: dict[str, set] = defaultdict(set)
    hash_domains: dict[str, set] = defaultdict(set)
    domains = set()
    pages = set()
    for r in rows:
        t = r["type"]
        n = r["bytes"]
        by_type[t].append(n)
        if r.get("body_sha256"):
            uniq_by_type[t][r["body_sha256"]] = n
            hash_splits[r["body_sha256"]].add(r["split"])
            hash_domains[r["body_sha256"]].add(r["domain"])
        domains_by_type[t].add(r["domain"])
        domains.add(r["domain"])
        if r.get("page"):
            pages.add(r["page"])
        split_rows[r["split"]] += 1
        split_bytes[r["split"]] += n
        split_domains[r["split"]].add(r["domain"])
        split_type_rows[r["split"]][t] += 1
        buckets[t][size_bucket(n)] += 1
        for fl in r.get("flags", []):
            flags[fl] += 1
    all_sizes = [r["bytes"] for r in rows]
    out = {
        **extra,
        "rows": len(rows),
        "bytes": sum(all_sizes),
        "distinct_bodies": len({r["body_sha256"] for r in rows if r.get("body_sha256")}),
        "distinct_body_bytes": sum(sum(v.values()) for v in uniq_by_type.values()),
        "distinct_domains": len(domains),
        "distinct_pages": len(pages),
        "domain_method": psl_method(),
        "split_rule": SPLIT_RULE,
        "by_type": {
            t: {
                **size_stats(v),
                "distinct_bodies": len(uniq_by_type[t]),
                "distinct_body_bytes": sum(uniq_by_type[t].values()),
                "distinct_domains": len(domains_by_type[t]),
                "size_buckets": dict(buckets[t]),
            }
            for t, v in sorted(by_type.items())
        },
        "all_sizes": size_stats(all_sizes),
        "splits": {
            s: {
                "rows": split_rows[s],
                "bytes": split_bytes[s],
                "domains": len(split_domains[s]),
                "rows_by_type": dict(split_type_rows[s]),
            }
            for s in ("train", "tune", "heldout")
        },
        "bodies_spanning_splits": sum(1 for v in hash_splits.values() if len(v) > 1),
        "bodies_on_multiple_domains": sum(1 for v in hash_domains.values() if len(v) > 1),
        "flags": dict(sorted(flags.items())),
    }
    with open(os.path.join(slice_dir, "summary.json"), "w") as f:
        json.dump(out, f, indent=2, sort_keys=False)
        f.write("\n")
    return out
