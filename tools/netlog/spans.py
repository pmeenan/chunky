"""chunky cross-session corpus pass (D-022): rule-3 excluded spans, template markers, repeat labels.

Runs over every session directory under <corpus_dir>/sessions/ (each written by
extract.py) and is re-runnable: re-run it whenever a session is added or
re-extracted. It rewrites each manifest.jsonl in place and writes
<corpus_dir>/templates.json. Run slices.py afterwards.

usage: python3 -I tools/netlog/spans.py <corpus_dir>
e.g.   python3 -I tools/netlog/spans.py data/corpus

What it writes:
  manifest rows   excluded_spans: the "session" rule spans are replaced (rules
                  "carrier" and "cookie" from extract.py are kept), then merged
                  and excluded_summary recomputed.
                  duplicate_of: "<session>/<id>" of the earliest row (session
                  order, then capture time) with the same body_sha256, else null.
                  repeat_of: "<session>/<id>" of the earliest earlier unflagged
                  row with the same repeat_key, else null (D-015 naturally
                  repeated bodies: panel queries recur in every session).
  templates.json  per session and site: template markers (Google: /search*
                  dictionary hashes, xjs build ids, kBL labels, rs= bundle
                  hashes; Etsy: hashed /ac/ asset names), template_change versus
                  the previous session of that site (dictionary or build-marker
                  change; null for a site's first session), and rule-3 stats.

Rule 3 (D-022 "values that repeat within a session but differ across
sessions"): tokens are maximal [A-Za-z0-9_-] runs of >= TOKEN_MIN bytes, with a
leading %XX, \\xXX or \\uXXXX escape remainder stripped. A token is a session
value of (session S, site) when it occurs in >= 2 distinct bodies of S and in
no body of any other session of that site. Every exact occurrence of it in S's
bodies of that site becomes a "session" span. Tokens that are also template
markers are counted as deploy markers (still excluded). A site captured in only
one session has no comparator; its rule-3 spans stay empty until a second
session of that site exists.
"""
import collections, json, os, re, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from extract import merge_spans, summarize_spans  # noqa: E402

TOKEN_MIN = 8
TOK = re.compile(rb'[A-Za-z0-9_\-]{%d,}' % TOKEN_MIN)
ESC = ((b'%', re.compile(rb'[0-9A-Fa-f]{2}')), (b'\\', re.compile(rb'x[0-9A-Fa-f]{2}|u[0-9A-Fa-f]{4}')))
MARKERS = {
    'google': {'build_ids': re.compile(rb'k=xjs\.\w+\.([\w\-]{8,})\.'),
               'kBL': re.compile(rb'\bkBL\s*[:=]\s*\\?[\'"]([\w\-]+)'),
               'rs': re.compile(rb'\brs(?:=|\\x3d|\\u003d)(A[\w\-]{20,})')},
    'etsy': {'asset_hashes': re.compile(rb'www\.etsy\.com\\?/ac\\?/\w+\\?/(?:js|css)\\?/[^"\'\s]*?([\w\-]+\.[\w\-]{6,40})\.(?:js|css)\b')},
}
BUILD_MARKER = {'google': 'build_ids', 'etsy': 'asset_hashes'}   # the marker whose majority change means a deploy


def tokens(b):
    for m in TOK.finditer(b):
        t, i = m.group(0), m.start()
        for pre, rx in ESC:
            if i and b[i - 1:i] == pre:
                e = rx.match(t)
                if e:
                    t = t[e.end():]
                break
        if len(t) >= TOKEN_MIN:
            yield t


def load_sessions(sroot):
    out = []
    for name in sorted(os.listdir(sroot)):
        d = os.path.join(sroot, name)
        mp = os.path.join(d, 'manifest.jsonl')
        if not os.path.isfile(mp):
            continue
        rows = [json.loads(l) for l in open(mp)]
        sj = os.path.join(d, 'session.json')
        meta = json.load(open(sj)) if os.path.isfile(sj) else {}
        start = meta.get('capture_start_utc') or min((r['captured_utc'] for r in rows), default='')
        out.append({'name': name, 'dir': d, 'rows': rows, 'start': start})
    out.sort(key=lambda s: (s['start'], s['name']))
    return out


def search_dicts(sdir):
    """sha256 SF values of the session's dictionaries that apply to /search."""
    out = set()
    dd = os.path.join(sdir, 'dicts')
    for f in sorted(os.listdir(dd)) if os.path.isdir(dd) else []:
        if f.endswith('.json'):
            j = json.load(open(os.path.join(dd, f)))
            m = re.search(r'match="([^"]*)"', j.get('use_as_dictionary', ''))
            if m and (m.group(1).startswith('/search') or m.group(1) == '/*'):
                out.add((j['sha256_sfv'], m.group(1)))
    return out


def ranked(counter):
    """Counter items by descending count, ties by key (deterministic across runs)."""
    return sorted(counter.items(), key=lambda kv: (-kv[1], kv[0]))


def majority(counter):
    return ranked(counter)[0][0] if counter else None


def main(corpus):
    sroot = os.path.join(corpus, 'sessions')
    sessions = load_sessions(sroot)
    bodies = {}         # (session, body_sha) -> bytes
    groups = collections.defaultdict(list)   # (session, site) -> rows
    for s in sessions:
        for r in s['rows']:
            groups[(s['name'], r['site'])].append(r)
            k = (s['name'], r['body_sha256'])
            if k not in bodies:
                bodies[k] = open(os.path.join(s['dir'], 'bodies', r['body_sha256'] + '.html'), 'rb').read()
    # Token -> number of distinct bodies, per (session, site); template markers per (session, site).
    tok = {}
    markers = {}
    for (sname, site), rows in groups.items():
        c, mk, seen = collections.Counter(), collections.defaultdict(collections.Counter), set()
        for r in rows:
            # Flagged rows (interstitial/captcha/consent) are outside every
            # slice (D-022), so they neither create nor veto rule-3 values.
            if r['flags'] or r['body_sha256'] in seen:
                continue
            seen.add(r['body_sha256'])
            b = bodies[(sname, r['body_sha256'])]
            c.update(set(tokens(b)))
            for name, rx in MARKERS.get(site, {}).items():
                mk[name].update({m.group(1).decode() for m in rx.finditer(b)})
            if r['dictionary_sha256']:
                mk['dictionaries_used'][r['dictionary_sha256']] += 1
        tok[(sname, site)] = c
        markers[(sname, site)] = mk
    # Rule 3.
    values, r3stats = {}, {}
    for (sname, site), c in tok.items():
        others = [k for k in tok if k[1] == site and k[0] != sname]
        if not others:
            values[(sname, site)] = set()
            r3stats[(sname, site)] = {'comparator_sessions': 0, 'status': 'pending: no other session of this site'}
            continue
        vals = {t for t, n in c.items() if n >= 2 and not any(t in tok[o] for o in others)}
        mk_vals = {v.encode() for name, cnt in markers[(sname, site)].items() if name != 'dictionaries_used' for v in cnt}
        deploy = {t for t in vals if t in mk_vals or any(t in v for v in mk_vals)}
        values[(sname, site)] = vals
        r3stats[(sname, site)] = {'comparator_sessions': len(others), 'values': len(vals), 'deploy_marker_values': len(deploy),
                                  'digit_only_values': sum(1 for t in vals if t.isdigit())}
    # Rewrite rows: spans and labels.
    first_body, first_key = {}, {}
    totals = collections.defaultdict(collections.Counter)
    for s in sessions:
        for r in sorted(s['rows'], key=lambda r: r['captured_utc']):
            ref = '%s/%d' % (s['name'], r['id'])
            r['duplicate_of'] = first_body.get(r['body_sha256'])
            first_body.setdefault(r['body_sha256'], ref)
            r['repeat_of'] = first_key.get(r['repeat_key'])
            if not r['flags']:
                first_key.setdefault(r['repeat_key'], ref)
            b = bodies[(s['name'], r['body_sha256'])]
            spans = [tuple(x) for x in r['excluded_spans'] if x[2] != 'session']
            for v in values[(s['name'], r['site'])]:
                i = b.find(v)
                while i >= 0:
                    spans.append((i, i + len(v), 'session'))
                    i = b.find(v, i + 1)
            r['excluded_spans'] = merge_spans(spans)
            r['excluded_summary'] = summarize_spans(r['excluded_spans'])
            t = totals[(s['name'], r['site'])]
            t['rows'] += 1
            t['flagged'] += bool(r['flags'])
            if r['flags']:
                continue
            t['decoded'] += r['decoded_bytes']
            t['duplicate'] += r['duplicate_of'] is not None
            t['repeat'] += r['repeat_of'] is not None
            t['repeat_panel'] += r['repeat_of'] is not None and r.get('panel', False)
            for rule, d in r['excluded_summary'].items():
                t[rule] += d['bytes']
        tmp = os.path.join(s['dir'], 'manifest.jsonl.tmp')
        with open(tmp, 'w') as f:
            for r in s['rows']:
                f.write(json.dumps(r) + '\n')
        os.replace(tmp, os.path.join(s['dir'], 'manifest.jsonl'))
    # templates.json
    prev, out = {}, []
    for s in sessions:
        sites = {}
        for site in sorted({r['site'] for r in s['rows']}):
            mk = markers[(s['name'], site)]
            entry = {'rows': len(groups[(s['name'], site)])}
            if site == 'google':
                entry['search_dictionaries'] = sorted('%s %s' % x for x in search_dicts(s['dir']))
            for name, cnt in sorted(mk.items()):
                entry[name] = dict(ranked(cnt))
            p = prev.get(site)
            if p is None:
                entry['previous_session'], entry['template_change'], entry['changes'] = None, None, []
            else:
                pmk = markers[(p, site)]
                changes = []
                if site == 'google' and {majority(pmk['dictionaries_used'])} != {majority(mk['dictionaries_used'])}:
                    changes.append('dictionary')
                bm = BUILD_MARKER[site]
                if site == 'google':
                    build_changed = majority(pmk[bm]) != majority(mk[bm])
                else:   # Etsy: any change in the set of hashed assets
                    build_changed = set(pmk[bm]) != set(mk[bm])
                if build_changed:
                    changes.append('build')
                entry['previous_session'], entry['template_change'], entry['changes'] = p, bool(changes), changes
            entry['rule3'] = r3stats[(s['name'], site)]
            sites[site] = entry
            prev[site] = s['name']
        out.append({'session': s['name'], 'capture_start_utc': s['start'], 'sites': sites})
    json.dump({'generated_by': 'tools/netlog/spans.py', 'token_min_bytes': TOKEN_MIN, 'sessions': out},
              open(os.path.join(corpus, 'templates.json'), 'w'), indent=1)
    # Report.
    site_tot = collections.defaultdict(collections.Counter)
    for (sname, site), t in sorted(totals.items()):
        site_tot[site].update(t)
        print_line('%s %s' % (sname, site), t)
    for site, t in sorted(site_tot.items()):
        print_line('ALL %s' % site, t)
    for e in out:
        for site, x in e['sites'].items():
            print(e['session'], site, 'template_change=%s %s' % (x['template_change'], x['changes']), 'rule3=%s' % x['rule3'])


def print_line(label, t):
    pct = {k: '%.3f%%' % (100 * t[k] / t['decoded']) for k in ('carrier', 'cookie', 'session', 'all')}
    print('%-18s rows=%3d flagged=%d dup=%d repeat=%d (panel %d)  decoded=%9d  %s' % (
        label, t['rows'], t['flagged'], t['duplicate'], t['repeat'], t['repeat_panel'], t['decoded'], pct))


if __name__ == '__main__':
    main(sys.argv[1])
