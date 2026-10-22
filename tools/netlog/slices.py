"""chunky corpus slices (D-015, D-022): chronological training / tuning / evaluation sessions.

Reads every session under <corpus_dir>/sessions/ (after spans.py) plus
<corpus_dir>/templates.json and writes <corpus_dir>/slices.json. Re-runnable.

usage: python3 -I tools/netlog/slices.py <corpus_dir> [--freeze ISO_UTC_TIME]
e.g.   python3 -I tools/netlog/slices.py data/corpus

Policy (the assignment is a pure function of the session list, the sessions'
capture start times and frozen_at):
  - Sessions are the unit, assigned per site (a session with Google and Etsy
    pages is in one slice per site; artifacts are partitioned per site, D-013).
  - Pre-freeze sessions (all of them while frozen_at is null) are split
    chronologically: the earliest 75% are training and the latest 25% tuning
    (D-022's 60:20), at least one tuning session once a site has two. Until the
    freeze the boundary may move as sessions are added; nothing has been
    evaluated on them.
  - Evaluation holds only sessions captured at or after frozen_at, i.e. after
    the cut rules and models are frozen (D-015). While frozen_at is null it is
    empty.
  - --freeze sets frozen_at once and records the pre-freeze sessions per site
    (frozen_sessions). Later runs split only that recorded list, so adding
    sessions never reshuffles frozen assignments; a session captured before
    frozen_at but not in the list is reported as unassigned.
  - Template-change sessions (templates.json: dictionary or build-marker change
    versus the previous session of the site) are listed per site as a separate
    slice; they keep their chronological assignment too.
  - Rows flagged interstitial / captcha / consent are excluded from every
    slice. Duplicate bodies (duplicate_of) and repeats (repeat_of, panel
    queries) are counted separately per slice.
"""
import argparse, collections, json, os, sys

TUNE_FRACTION = 0.25    # of pre-freeze sessions: 60:20 of D-022's 60/20/20
POLICY = [
    'sessions are the unit, assigned per site in capture order',
    'pre-freeze sessions: earliest 75% training, latest 25% tuning (at least one tuning session once a site has two)',
    'evaluation: only sessions captured at or after frozen_at; empty while frozen_at is null',
    'after --freeze, frozen_sessions fixes the pre-freeze split; adding sessions never reshuffles it',
    'template_change lists sessions whose dictionary or build markers changed versus the previous session of the site',
    'rows flagged interstitial/captcha/consent are excluded from every slice; duplicates and repeats are counted separately',
]


def split_pre_freeze(names):
    """Chronologically ordered pre-freeze session names -> (training, tuning)."""
    n = len(names)
    n_tune = max(1, int(n * TUNE_FRACTION + 0.5)) if n >= 2 else 0
    return names[:n - n_tune], names[n - n_tune:]


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('corpus')
    ap.add_argument('--freeze', help='set frozen_at (UTC ISO time); only allowed once')
    a = ap.parse_args(argv)
    sroot = os.path.join(a.corpus, 'sessions')
    out_path = os.path.join(a.corpus, 'slices.json')
    old = json.load(open(out_path)) if os.path.isfile(out_path) else {}
    frozen_at, frozen_sessions = old.get('frozen_at'), old.get('frozen_sessions')
    if a.freeze:
        if frozen_at:
            sys.exit('already frozen at %s' % frozen_at)
        frozen_at = a.freeze
    templates = {}
    tp = os.path.join(a.corpus, 'templates.json')
    if os.path.isfile(tp):
        for e in json.load(open(tp))['sessions']:
            templates[e['session']] = e['sites']
    sessions = []
    for name in sorted(os.listdir(sroot)):
        mp = os.path.join(sroot, name, 'manifest.jsonl')
        if not os.path.isfile(mp):
            continue
        rows = [json.loads(l) for l in open(mp)]
        sj = os.path.join(sroot, name, 'session.json')
        meta = json.load(open(sj)) if os.path.isfile(sj) else {}
        start = meta.get('capture_start_utc') or min(r['captured_utc'] for r in rows)
        sessions.append((start, name, rows))
    sessions.sort()
    by_site = collections.defaultdict(list)
    for start, name, rows in sessions:
        for site in sorted({r['site'] for r in rows}):
            by_site[site].append((start, name))
    if a.freeze:
        frozen_sessions = {site: [n for s, n in lst if s < frozen_at] for site, lst in by_site.items()}
    sites = {}
    for site, lst in sorted(by_site.items()):
        if frozen_at:
            pre = (frozen_sessions or {}).get(site, [])
            evaluation = [n for s, n in lst if s >= frozen_at]
            unassigned = [n for s, n in lst if s < frozen_at and n not in pre]
        else:
            pre, evaluation, unassigned = [n for _, n in lst], [], []
        training, tuning = split_pre_freeze(pre)
        sites[site] = {'training': training, 'tuning': tuning, 'evaluation': evaluation,
                       'template_change': [n for _, n in lst if templates.get(n, {}).get(site, {}).get('template_change')],
                       'unassigned': unassigned}
    # Row counts per slice; excluded rows.
    excluded, slice_of = [], {}
    for site, sl in sites.items():
        for k in ('training', 'tuning', 'evaluation'):
            for n in sl[k]:
                slice_of[(site, n)] = k
    counts = collections.defaultdict(lambda: collections.defaultdict(collections.Counter))
    for start, name, rows in sessions:
        for r in rows:
            if r['flags']:
                excluded.append({'row': '%s/%d' % (name, r['id']), 'site': r['site'], 'flags': r['flags']})
                continue
            k = slice_of.get((r['site'], name), 'unassigned')
            c = counts[r['site']][k]
            c['rows'] += 1
            c['decoded_bytes'] += r['decoded_bytes']
            c['route ' + r['route']] += 1
            if r.get('duplicate_of'):
                c['duplicate'] += 1
            elif r.get('repeat_of'):
                c['repeat (panel)' if r.get('panel') else 'repeat'] += 1
            else:
                c['first occurrence'] += 1
    for site, sl in sites.items():
        sl['rows'] = {k: dict(sorted(v.items())) for k, v in sorted(counts[site].items())}
    out = {'generated_by': 'tools/netlog/slices.py',
           'policy': POLICY,
           'frozen_at': frozen_at,
           'frozen_sessions': frozen_sessions if frozen_at else None,
           'rule_after_freeze': 'sessions captured at or after frozen_at go to evaluation; frozen_sessions keep their split',
           'sessions': [{'session': n, 'capture_start_utc': s} for s, n, _ in sessions],
           'sites': sites,
           'excluded_rows': excluded}
    json.dump(out, open(out_path, 'w'), indent=1)
    print(json.dumps(out, indent=1))


if __name__ == '__main__':
    main(sys.argv[1:])
