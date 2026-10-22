"""chunky netlog extractor (D-018, D-022).

Turns one capture session's Chrome netlog (capture mode Everything) into a
session directory: per-response manifest rows for the approved documents,
their served and decoded bodies, the Use-As-Dictionary dictionaries they use,
and a session.json. Cookie and similar header values are redacted. Each body
is decoded independently (C libbrotlidec with the dictionary attached for dcb,
zlib for gzip) and checked against the netlog's decoded bytes. brotli flush
boundaries are byte-aligned meta-block ends proposed by `chunky flushes` and
each validated with the C decoder; gzip flushes are sync/full-flush block ends
validated by re-inflation.

Approved sources: www.google.com/search and Etsy listing, category and shop
pages (D-018, D-022). Other documents in the netlog are skipped.

usage: python3 -I tools/netlog/extract.py <netlog.json> <out_dir> <chunky_binary>
           [--region US] [--method manual|automated] [--panel tools/netlog/panel.txt]
e.g.   hostlock shared --label "chunky: extract" -- python3 -I tools/netlog/extract.py \
           data/netlogs/netlog-1022.json data/corpus/sessions/2026-10-22 target/release/chunky
then   python3 -I tools/netlog/spans.py data/corpus      (cross-session pass, all sessions)
       python3 -I tools/netlog/slices.py data/corpus     (data/corpus/slices.json)

Build the binary with `hostlock shared -- cargo build --release -p chunky-cli`.
Needs the system libbrotlidec (C brotli >= 1.1.0, for dictionary attach).
Raw netlogs hold credential-equivalent data: keep them and all output under
data/ (git-ignored, D-007/D-018). Cookie values are read in memory only to mark
spans; no value is written out.

Output (one directory per session, the session id is the directory name):
  manifest.jsonl          one row per approved document (fields below)
  session.json            session metadata (D-022): capture date/times, Chrome
                          version and host OS (netlog clientInfo), emulated
                          device, user agent, locale, region (Etsy
                          detected_region, else --region), method, per-site
                          counts, source netlog name/size/sha256, extractor
                          version, and the excluded-span rule parameters
  bodies/<sha256>.html    decoded body
  served/<id>.<encoding>  served (encoded) body
  dicts/<sha256>.{dict,json}  dictionary and its Use-As-Dictionary metadata

Manifest row (manifest_version 1; M1 adds the fields marked +):
  manifest_version, id (netlog source id), captured_utc, site, url, query,
  route ("/search[?udm=N] mobile", "/listing mobile", ...), device, protocol,
  status_line, request_headers, response_headers (cookie-class values
  redacted), served_encoding, decode_verified_by, dictionary_sha256 (SF
  binary, dcb only), served_bytes, decoded_bytes, body_sha256, served_file,
  transport_reads [[ms, bytes]], flushes [{decoded_offset, stream_offset,
  arrival_ms}], flags (consent / captcha / interstitial: excluded from every
  slice)
  + repeat_key     grouping key for the same request across sessions:
                   "google|<route>|<normalized query>" or "etsy|<route>|<path>"
  + panel          true when the query is in tools/netlog/panel.txt (D-022
                   panel queries; report separately from new queries)
  + repeat_of      "<session>/<id>" of the earliest earlier unflagged row with
                   the same repeat_key (null if none); filled by spans.py
  + duplicate_of   "<session>/<id>" of the earliest row with a byte-identical
                   body (null if none); filled by spans.py
  + excluded_spans [[start, end, rule], ...] in decoded-byte offsets, end
                   exclusive, sorted, merged per rule (spans of different rules
                   may overlap). Rules (D-022 "Excluded spans"):
                     carrier  named per-response carriers (CARRIERS below), plus
                              every other exact occurrence of a carrier value of
                              >= PROPAGATE_MIN bytes in the same body
                     cookie   exact occurrences of any cookie / Set-Cookie value
                              (and its percent-decoded form) of >= COOKIE_MIN
                              bytes seen anywhere in the session's netlog (request
                              and response headers on every connection, and the
                              cookie store), except URL- or host-shaped values
                     session  values repeated within a session but absent from
                              other sessions of the same site; written by
                              spans.py (re-run it whenever sessions are added)
  + excluded_summary {rule: {count, bytes}, all: {bytes}}; "all" is the union
"""
import argparse, base64, collections, ctypes, datetime, hashlib, json, os, re, subprocess, sys, tempfile, urllib.parse, zlib

EXTRACTOR_VERSION = 'm1.1'
REDACT = {'cookie', 'set-cookie', 'x-client-data', 'authorization', 'proxy-authorization'}
DCB_MAGIC = b'\xffDCB'
COOKIE_MIN = 8       # cookie values shorter than this are not matched (too likely to hit content)
PROPAGATE_MIN = 8    # carrier values at least this long are also excluded wherever else they occur

# --- Named per-response carriers (D-022 rule 1). Each regex's group 'v' is the value span.
# Values appear in HTML attributes, in JS strings (\x3d / \x22 / \u003d / \" escapes) and in
# URL query strings (&amp;, \x26, \u0026 separators).
_EQ = rb'(?:=|\\x3d|\\u003d)'
_Q = rb'(?:\\?["\']|\\x22|\\x27|\\u0022|&quot;)?'
_SEP = rb'(?:[?&;]|\\x26|\\u0026)'
_PV = rb'(?:[\w\-:]|%(?!26)[0-9A-Fa-f]{2})+'      # query-parameter value (stops at & and %26)


def _attr(name, val=rb'[^"\'\\<>\s]+'):
    return rb'\b' + name + _EQ + _Q + rb'(?P<v>' + val + rb')'


def _param(name, val=_PV):
    return _SEP + name + _EQ + rb'(?P<v>' + val + rb')'


def _key(name, val=rb'[^"\\]+'):
    return rb'\\?"' + name + rb'\\?"\s*:\s*\\?"(?P<v>' + val + rb')'


CARRIERS = [  # (site or '*', carrier name, regex)
    ('*', 'csp-nonce', _attr(rb'nonce', rb'[A-Za-z0-9+/_\-]{8,}={0,2}')),
    ('google', 'kEI', rb'\bkEI\s*[:=]\s*\\?[\'"](?P<v>[\w\-]{10,})'),
    ('google', 'ei', _param(rb's?ei', rb'[\w\-]{10,}')),
    ('google', 'ved', _attr(rb'data-ved', rb'[\w\-]{10,}')),   # short static forms ("1", "1t:200715") excluded
    ('google', 'ved', _param(rb'ved', rb'[\w\-]{10,}')),
    ('google', 'generated-id', _attr(rb'id', rb'_[\w\-]{8,}')),
    ('google', 'goto-url', rb'/goto\?url' + _EQ + rb'(?P<v>' + _PV + rb')'),
    # Beyond the D-022 list: XSRF token, signed /url? redirect parameters, search-state token.
    ('google', 'sxsrf', _param(rb'sxsrf')),
    ('google', 'url-signed', _param(rb'url', rb'CAES[\w\-]+')),
    ('google', 'usg', _param(rb'usg')),
    ('google', 'data-sstk', _attr(rb'data-sstk', rb'[\w\-]{16,}')),
    # Etsy (survey of the 2026-10-22 session's 48 pages).
    ('etsy', 'csrf-nonce', rb'(?:csrf|uaid)_nonce"\s+content="(?P<v>[^"]+)'),
    ('etsy', 'csrf-nonce', _key(rb'(?:csrf|uaid)_nonce')),
    ('etsy', 'csrf-nonce', rb'name="_nnc"\s+value="(?P<v>[^"]+)'),
    ('etsy', 'csrf-nonce', rb'(?P<v>\b3:\d{10}:[\w\-]{16,}:[0-9a-f]{16,})'),   # Etsy signed-nonce shape anywhere
    ('etsy', 'request-id', _key(rb'request_uuid')),
    ('etsy', 'request-id', _key(rb'page_guid')),
    ('etsy', 'request-id', _key(rb'guid', rb'[0-9a-f]{8,}\.[0-9a-f]{8,}\.\d+')),
    ('etsy', 'request-id', _key(rb'digest', rb'[0-9a-f]{20,}')),
    ('etsy', 'request-id', rb'"uuid":\s*(?P<v>\d{9,})'),
    ('etsy', 'workflow', _param(rb'workflow')),
    ('etsy', 'client-derived', _key(rb'header_signature')),
    ('etsy', 'client-derived', _key(rb'ip_org')),
    ('etsy', 'client-derived', rb'"device_identifier":\{"source":"[^"]*","value":"(?P<v>[^"]+)'),   # uaid echo
]
CARRIER_RES = [(s, n, re.compile(p)) for s, n, p in CARRIERS]

lib = ctypes.CDLL('libbrotlidec.so.1')
lib.BrotliDecoderCreateInstance.restype = ctypes.c_void_p
lib.BrotliDecoderCreateInstance.argtypes = [ctypes.c_void_p] * 3
lib.BrotliDecoderAttachDictionary.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_size_t, ctypes.c_char_p]
lib.BrotliDecoderDecompressStream.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_void_p),
                                              ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p]
lib.BrotliDecoderDestroyInstance.argtypes = [ctypes.c_void_p]


def c_decode(stream, dictionary=b''):
    """Decode a complete brotli stream with the C decoder; None unless it ends with SUCCESS."""
    st = lib.BrotliDecoderCreateInstance(None, None, None)
    keep = ctypes.create_string_buffer(dictionary, len(dictionary)) if dictionary else None
    if dictionary and not lib.BrotliDecoderAttachDictionary(st, 0, len(dictionary), keep):
        raise RuntimeError('attach failed')
    inbuf = ctypes.create_string_buffer(stream, len(stream))
    out = bytearray()
    obuf = ctypes.create_string_buffer(1 << 20)
    avail_in = ctypes.c_size_t(len(stream))
    next_in = ctypes.c_void_p(ctypes.addressof(inbuf))
    try:
        while True:
            avail_out = ctypes.c_size_t(len(obuf))
            next_out = ctypes.c_void_p(ctypes.addressof(obuf))
            r = lib.BrotliDecoderDecompressStream(st, ctypes.byref(avail_in), ctypes.byref(next_in), ctypes.byref(avail_out), ctypes.byref(next_out), None)
            out += obuf.raw[:len(obuf) - avail_out.value]
            if r == 3:
                continue
            return bytes(out) if (r == 1 and avail_in.value == 0) else None
    finally:
        lib.BrotliDecoderDestroyInstance(st)


def split_header(h):
    if not isinstance(h, str) or ': ' not in h:
        return None
    k, v = h.split(': ', 1)
    return k.lower(), v


def headers(lst):
    out = []
    for h in lst:
        kv = split_header(h)
        if kv:
            out.append([kv[0], '<redacted %d bytes>' % len(kv[1]) if kv[0] in REDACT else kv[1]])
    return out


def first(hs, k):
    return next((v for kk, v in hs if kk == k), None)


def cookie_values_from_header(k, v, out):
    """Add the values of a Cookie or Set-Cookie header to out (a set of str)."""
    if k == 'cookie':
        crumbs = v.split(';')
    elif k == 'set-cookie':
        crumbs = v.split('\n')            # netlog may fold several Set-Cookie lines
        crumbs = [c.split(';', 1)[0] for c in crumbs]
    else:
        return
    for c in crumbs:
        if '=' in c:
            out.add(c.split('=', 1)[1].strip().strip('"'))


def is_url_like(v):
    """Cookie values that are URLs or host names (navigation state such as Etsy last_browse_page,
    Google SEOD/SEOR) would match ordinary links in every page; they are not matched."""
    d = urllib.parse.unquote(v)
    return '://' in d or re.fullmatch(r'[a-z0-9.\-]+\.[a-z]{2,}', d) is not None


def norm_query(q):
    return ' '.join(q.lower().split())


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('netlog')
    ap.add_argument('out_dir')
    ap.add_argument('chunky', help='chunky binary (runs `<chunky> flushes ...`)')
    ap.add_argument('--region', default='US', help='session region when no page reports one (default US)')
    ap.add_argument('--method', default='manual', choices=('manual', 'automated'))
    ap.add_argument('--panel', default=os.path.join(os.path.dirname(os.path.abspath(__file__)), 'panel.txt'))
    a = ap.parse_args(argv)
    panel = {norm_query(l) for l in open(a.panel) if l.strip() and not l.startswith('#')}
    out_dir = a.out_dir
    log = json.load(open(a.netlog))
    c = log['constants']
    ev = {v: k for k, v in c['logEventTypes'].items()}
    st = {v: k for k, v in c['logSourceType'].items()}
    tick0 = int(c['timeTickOffset'])
    reqs = collections.defaultdict(list)
    cookies = set()   # every cookie / Set-Cookie value in the netlog: memory only, never written
    for e in log['events']:
        p = e.get('params') or {}
        if isinstance(p.get('headers'), list):
            for h in p['headers']:
                kv = split_header(h)
                if kv:
                    cookie_values_from_header(kv[0], kv[1], cookies)
        if ev[e['type']] in ('COOKIE_STORE_COOKIE_ADDED', 'COOKIE_STORE_COOKIE_DELETED') and isinstance(p.get('value'), str):
            cookies.add(p['value'])
        if st.get(e['source']['type']) == 'URL_REQUEST':
            reqs[e['source']['id']].append(e)
    client_info = c.get('clientInfo', {})
    del log
    cookie_needles, n_url_cookies = set(), 0
    for v in cookies:
        if is_url_like(v):
            n_url_cookies += 1
            continue
        for x in (v, urllib.parse.unquote(v)):
            if len(x.encode()) >= COOKIE_MIN:
                cookie_needles.add(x.encode())
    recs = []
    for sid, es in reqs.items():
        r = {'id': sid, 'url': None, 'req': [], 'resp': [], 'raw': [], 'dec': [], 'start': None, 'proto': None}
        for e in es:
            n, p, t = ev[e['type']], e.get('params', {}), int(e['time'])
            if n == 'URL_REQUEST_START_JOB' and r['url'] is None:
                r['url'], r['method'], r['start'] = p.get('url'), p.get('method'), t
            elif n.endswith('SEND_REQUEST_HEADERS') and 'headers' in p and n.startswith('HTTP_TRANSACTION'):
                r['req'] = headers(p['headers'])
                r['proto'] = {'HTTP_TRANSACTION_QUIC_SEND_REQUEST_HEADERS': 'h3', 'HTTP_TRANSACTION_HTTP2_SEND_REQUEST_HEADERS': 'h2'}.get(n, 'http/1.1')
            elif n == 'HTTP_TRANSACTION_READ_RESPONSE_HEADERS':
                r['status_line'] = (p.get('headers') or [''])[0]
                r['resp'] = headers(p.get('headers', [])[1:])
            elif n == 'URL_REQUEST_JOB_BYTES_READ':
                r['raw'].append((t, base64.b64decode(p.get('bytes', ''))))
            elif n == 'URL_REQUEST_JOB_FILTERED_BYTES_READ':
                r['dec'].append((t, base64.b64decode(p.get('bytes', ''))))
        if r['url']:
            recs.append(r)
    del reqs
    os.makedirs(os.path.join(out_dir, 'bodies'), exist_ok=True)
    os.makedirs(os.path.join(out_dir, 'served'), exist_ok=True)
    os.makedirs(os.path.join(out_dir, 'dicts'), exist_ok=True)
    # Dictionaries: Use-As-Dictionary responses from www.google.com, stored by sha-256.
    dicts = {}
    for r in recs:
        uad = first(r['resp'], 'use-as-dictionary')
        if uad and urllib.parse.urlsplit(r['url']).netloc == 'www.google.com':
            body = b''.join(b for _, b in r['dec'])
            h = hashlib.sha256(body).digest()
            sfv = ':%s:' % base64.b64encode(h).decode()
            dicts[sfv] = body
            open(os.path.join(out_dir, 'dicts', h.hex() + '.dict'), 'wb').write(body)
            json.dump({'sha256_sfv': sfv, 'url': r['url'], 'use_as_dictionary': uad, 'content_type': first(r['resp'], 'content-type'),
                       'served_encoding': first(r['resp'], 'content-encoding'), 'bytes': len(body),
                       'captured_utc': utc(tick0, r['start'])},
                      open(os.path.join(out_dir, 'dicts', h.hex() + '.json'), 'w'), indent=1)
    manifest = open(os.path.join(out_dir, 'manifest.jsonl'), 'w')
    report = collections.Counter()
    meta = collections.defaultdict(collections.Counter)
    span_bytes = collections.defaultdict(collections.Counter)
    for r in sorted(recs, key=lambda r: r['start']):
        u = urllib.parse.urlsplit(r['url'])
        if first(r['req'], 'sec-fetch-dest') != 'document':
            continue
        site, route = approved(u)
        if not site:
            report['skipped non-approved document'] += 1
            continue
        served = b''.join(b for _, b in r['raw'])
        decoded = b''.join(b for _, b in r['dec']) if r['dec'] else served
        ce = first(r['resp'], 'content-encoding')
        dict_sfv, dictionary, stream, hdr = None, b'', served, 0
        if ce == 'dcb':
            assert served[:4] == DCB_MAGIC, 'bad dcb magic'
            dict_sfv = ':%s:' % base64.b64encode(served[4:36]).decode()
            assert dict_sfv == first(r['req'], 'available-dictionary'), 'dcb hash != Available-Dictionary'
            dictionary, stream, hdr = dicts[dict_sfv], served[36:], 36
        flushes, verified = [], None
        if ce in ('br', 'dcb'):
            assert c_decode(stream, dictionary) == decoded, 'C decode != netlog decoded bytes for %s' % r['url']
            verified = 'C libbrotlidec'
            flushes = recover_flushes(stream, dictionary, decoded, hdr, r['raw'], a.chunky, report)
        elif ce == 'gzip':
            assert zlib.decompress(served, 16 + zlib.MAX_WBITS) == decoded, 'gzip decode != netlog decoded bytes'
            verified = 'zlib'
            hdr = gzip_header_len(served)
            flushes = recover_gzip_flushes(served, hdr, decoded, r['raw'], report)
        elif ce in (None, 'identity'):
            assert served == decoded
            verified = 'identity'
        report['verified %s' % (verified or 'no (%s)' % ce)] += 1
        # Arrival time of the read that completes each flushed prefix.
        ends, off = [], -hdr
        for t, b in r['raw']:
            off += len(b)
            ends.append((off, t - r['start']))

        def arrival(comp):
            return next(t for o, t in ends if o >= comp)
        body_sha = hashlib.sha256(decoded).hexdigest()
        open(os.path.join(out_dir, 'bodies', body_sha + '.html'), 'wb').write(decoded)
        open(os.path.join(out_dir, 'served', '%d.%s' % (r['id'], ce or 'identity')), 'wb').write(served)
        q = urllib.parse.parse_qs(u.query)
        low = decoded[:200000].lower()
        flags = [f for f, hit in (('consent', b'consent.google.com' in low and b'<form' in low and len(decoded) < 200000),
                                  ('captcha', u.path.startswith('/sorry') or b'unusual traffic' in low or b'captcha-delivery' in low),
                                  # Google's JavaScript-check interstitial: served before the real results
                                  # page (which follows with &sei=); it is not a results page.
                                  ('interstitial', site == 'google' and b'id="rso"' not in decoded and b'/httpservice/retry/enablejs' in decoded)) if hit]
        device = ' mobile' if first(r['req'], 'sec-ch-ua-mobile') == '?1' else ' desktop'
        query = q.get('q', [''])[0] if site == 'google' else ''
        spans = carrier_spans(site, decoded) + needle_spans(decoded, cookie_needles, 'cookie')
        spans = merge_spans(spans)
        summary = summarize_spans(spans)
        for rule, s in summary.items():
            span_bytes[site][rule] += s['bytes']
        span_bytes[site]['decoded'] += len(decoded)
        for k in ('user-agent', 'accept-language', 'sec-ch-ua-model', 'sec-ch-ua-mobile', 'sec-ch-ua-platform',
                  'sec-ch-ua-full-version-list', 'sec-ch-ua-platform-version'):
            v = first(r['req'], k)
            if v is not None:
                meta[k][v] += 1
        m = re.search(rb'"detected_region":"([A-Z]{2})"', decoded) if site == 'etsy' else None
        if m:
            meta['region'][m.group(1).decode()] += 1
        meta['site'][site] += 1
        meta['captured'][utc(tick0, r['start'])] += 1
        manifest.write(json.dumps({
            'manifest_version': 1, 'id': r['id'],
            'captured_utc': utc(tick0, r['start']),
            'site': site, 'url': r['url'], 'query': query, 'route': route + device,
            'device': (first(r['req'], 'sec-ch-ua-model') or first(r['req'], 'sec-ch-ua-platform') or '').strip('"'),
            'protocol': r['proto'], 'status_line': r['status_line'], 'request_headers': r['req'], 'response_headers': r['resp'],
            'served_encoding': ce, 'decode_verified_by': verified, 'dictionary_sha256': dict_sfv,
            'served_bytes': len(served), 'decoded_bytes': len(decoded),
            'body_sha256': body_sha, 'served_file': 'served/%d.%s' % (r['id'], ce or 'identity'),
            'transport_reads': [[t - r['start'], len(b)] for t, b in r['raw']],
            'flushes': [{'decoded_offset': d, 'stream_offset': s, 'arrival_ms': arrival(s)} for s, d in flushes],
            'flags': flags,
            'repeat_key': '%s|%s|%s' % (site, route + device, norm_query(query) if site == 'google' else u.path),
            'panel': site == 'google' and norm_query(query) in panel,
            'repeat_of': None, 'duplicate_of': None,
            'excluded_spans': spans, 'excluded_summary': summary}) + '\n')
        label = query if site == 'google' else u.path
        print('%-34s %-4s %4s %8d -> %8d  flushes=%3d  reads=%3d  last_read=%5dms excl=%5.2f%% %s' % (
            label[:34], ce, r['proto'], len(served), len(decoded), len(flushes), len(r['raw']),
            r['raw'][-1][0] - r['start'] if r['raw'] else 0, 100 * summary['all']['bytes'] / max(1, len(decoded)), flags or ''))
    manifest.close()
    print(dict(report))
    for site, sb in sorted(span_bytes.items()):
        print(site, {k: '%.3f%%' % (100 * v / sb['decoded']) for k, v in sb.items() if k != 'decoded'}, 'of', sb['decoded'], 'decoded bytes')
    write_session(a, out_dir, client_info, meta, len(cookie_needles), n_url_cookies)


def utc(tick0, t):
    return datetime.datetime.fromtimestamp((tick0 + t) / 1000, datetime.timezone.utc).isoformat()


def top(counter):
    return counter.most_common(1)[0][0] if counter else None


def write_session(a, out_dir, client_info, meta, n_cookie_needles, n_url_cookies):
    """session.json (D-022 "Session metadata")."""
    h, size = hashlib.sha256(), 0
    with open(a.netlog, 'rb') as f:
        for blk in iter(lambda: f.read(1 << 22), b''):
            h.update(blk)
            size += len(blk)
    full = top(meta['sec-ch-ua-full-version-list']) or ''
    m = re.search(r'"Google Chrome";v="([\d.]+)"', full) or re.search(r'"Chromium";v="([\d.]+)"', full)
    ua = top(meta['user-agent']) or ''
    times = sorted(meta['captured'])
    script = open(os.path.abspath(__file__), 'rb').read()
    region = top(meta['region'])
    session_id = os.path.basename(os.path.normpath(out_dir))
    session = {
        'session': session_id,
        # The session id is the owner's local capture date; captured_utc times are UTC.
        'capture_date': session_id if re.fullmatch(r'\d{4}-\d\d-\d\d', session_id) else (times[0][:10] if times else None),
        'capture_start_utc': times[0] if times else None,
        'capture_end_utc': times[-1] if times else None,
        'chrome_version': client_info.get('version'),
        'chrome_ua_full_version': m.group(1) if m else (re.search(r'Chrome/([\d.]+)', ua).group(1) if 'Chrome/' in ua else None),
        'host_os': client_info.get('os_type'),
        'user_agent': ua,
        'device': {'model': (top(meta['sec-ch-ua-model']) or '').strip('"') or None,
                   'class': {'?1': 'mobile', '?0': 'desktop'}.get(top(meta['sec-ch-ua-mobile'])),
                   'platform': (top(meta['sec-ch-ua-platform']) or '').strip('"') or None,
                   'emulated': bool(client_info.get('os_type')) and 'Android' in ua and 'Android' not in client_info.get('os_type', '')},
        'locale': top(meta['accept-language']),
        'region': region or a.region,
        'region_source': 'etsy detected_region' if region else 'cli --region',
        'method': a.method,
        'sites': dict(meta['site']),
        'source_netlog': {'file': os.path.basename(a.netlog), 'bytes': size, 'sha256': h.hexdigest()},
        'extractor': {'version': EXTRACTOR_VERSION, 'script_sha256': hashlib.sha256(script).hexdigest()},
        'excluded_spans': {'cookie_min_bytes': COOKIE_MIN, 'cookie_values_matched': n_cookie_needles,
                           'cookie_values_skipped_url_like': n_url_cookies,
                           'carrier_propagate_min_bytes': PROPAGATE_MIN,
                           'carriers': sorted({'%s:%s' % (s, n) for s, n, _ in CARRIERS})},
    }
    json.dump(session, open(os.path.join(out_dir, 'session.json'), 'w'), indent=1)
    print(json.dumps(session, indent=1))


def carrier_spans(site, body):
    """Rule 1: value spans of the named carriers, plus other occurrences of their values."""
    spans, values = [], set()
    for s, name, rx in CARRIER_RES:
        if s not in ('*', site):
            continue
        for m in rx.finditer(body):
            if m.end('v') > m.start('v'):
                spans.append((m.start('v'), m.end('v'), 'carrier'))
                if m.end('v') - m.start('v') >= PROPAGATE_MIN:
                    values.add(m.group('v'))
    return spans + needle_spans(body, values, 'carrier')


def needle_spans(body, needles, rule):
    """Every exact occurrence of each needle (bytes) in body."""
    out = []
    for n in needles:
        i = body.find(n)
        while i >= 0:
            out.append((i, i + len(n), rule))
            i = body.find(n, i + 1)
    return out


def merge_spans(spans):
    """Sort and merge overlapping or touching spans per rule -> [[start, end, rule], ...]."""
    out, by_rule = [], collections.defaultdict(list)
    for s, e, rule in spans:
        by_rule[rule].append((s, e))
    for rule, lst in by_rule.items():
        lst.sort()
        cur = None
        for s, e in lst:
            if cur and s <= cur[1]:
                cur[1] = max(cur[1], e)
            else:
                cur = [s, e, rule]
                out.append(cur)
    out.sort(key=lambda x: (x[0], x[1], x[2]))
    return out


def summarize_spans(spans):
    """{rule: {count, bytes}, all: {bytes}} with "all" the union over rules."""
    summary = {}
    for s, e, rule in spans:
        d = summary.setdefault(rule, {'count': 0, 'bytes': 0})
        d['count'] += 1
        d['bytes'] += e - s
    union, end = 0, -1
    for s, e, _ in sorted(spans):
        if e > end:
            union += e - max(s, end)
            end = e
    summary['all'] = {'bytes': union}
    return summary


def approved(u):
    """(site, route) for approved sources (D-018, D-022), else (None, None)."""
    if u.netloc == 'www.google.com' and u.path == '/search':
        q = urllib.parse.parse_qs(u.query)
        return 'google', '/search' + ('?udm=' + q['udm'][0] if 'udm' in q else '')
    if u.netloc == 'www.etsy.com':
        for route, pat in (('/listing', r'/listing/\d+/[^/]+'), ('/c', r'/c/[a-z0-9\-/]+'), ('/shop', r'/shop/[A-Za-z0-9]+')):
            if re.fullmatch(pat, u.path):
                return 'etsy', route
    return None, None


def recover_flushes(stream, dictionary, decoded, hdr, raw_reads, chunky, report):
    """Byte-aligned meta-block ends proposed by `chunky flushes`, each validated with the C decoder."""
    with tempfile.TemporaryDirectory() as td:
        sp, dp, op = (os.path.join(td, x) for x in ('s', 'd', 'o'))
        open(sp, 'wb').write(stream)
        open(dp, 'wb').write(dictionary)
        res = subprocess.run([chunky, 'flushes', sp, dp if dictionary else '-', op], capture_output=True, text=True, check=True)
        assert open(op, 'rb').read() == decoded, 'rust decode mismatch'
    cands = [tuple(map(int, l.split())) for l in res.stdout.split('\n') if l]
    flushes = []
    for comp, dec in cands:
        if c_decode(stream[:comp] + b'\x03', dictionary) == decoded[:dec]:
            flushes.append((comp, dec))
        else:
            report['candidate rejected by C'] += 1
    # Recall check at every transport read boundary.
    fs = {x[0] for x in flushes}
    off = -hdr
    for _, b in raw_reads:
        off += len(b)
        if 0 < off < len(stream):
            ok = c_decode(stream[:off] + b'\x03', dictionary) is not None
            report['read boundary decodable'] += ok
            report['read boundary decodable but missed'] += ok and off not in fs
            report['read boundary mid-block'] += not ok
    return flushes


def gzip_header_len(b):
    """Length of the gzip member header (RFC 1952) before the deflate stream."""
    assert b[:2] == b'\x1f\x8b' and b[2] == 8, 'not a gzip/deflate member'
    flg, i = b[3], 10
    if flg & 4:
        i += 2 + int.from_bytes(b[i:i + 2], 'little')
    for bit in (8, 16):
        if flg & bit:
            i = b.index(b'\0', i) + 1
    if flg & 2:
        i += 2
    return i


def gzip_boundary(served, hdr, decoded, k):
    """Decoded offset if served[:k] ends at a byte-aligned deflate block boundary, else None.

    Exact check: a fresh raw inflater primed with the preceding 32 KiB of output
    must reproduce the rest of the body from served[k:]. That only works at a
    real block boundary (a sync/full flush), not at a chance 00 00 ff ff.
    """
    n = len(zlib.decompressobj(16 + zlib.MAX_WBITS).decompress(served[:k]))
    try:
        d = zlib.decompressobj(-zlib.MAX_WBITS, zdict=decoded[max(0, n - 32768):n])
        rest = d.decompress(served[k:]) + d.flush()
    except zlib.error:
        return None
    return n if decoded[:n] + rest == decoded else None


def recover_gzip_flushes(served, hdr, decoded, raw_reads, report):
    """Server flush points in a gzip body: byte-aligned block ends after a sync/full flush."""
    flushes, k = [], served.find(b'\x00\x00\xff\xff', hdr)
    while k >= 0:
        end = k + 4
        if end < len(served) - 8:
            n = gzip_boundary(served, hdr, decoded, end)
            if n is None:
                report['gzip candidate rejected'] += 1
            elif not flushes or flushes[-1][1] != n:
                flushes.append((end - hdr, n))
        k = served.find(b'\x00\x00\xff\xff', k + 1)
    if not flushes or flushes[-1][1] != len(decoded):
        flushes.append((len(served) - 8 - hdr, len(decoded)))   # end of the final block, before the trailer
    fs = {f[0] + hdr for f in flushes}
    off = 0
    for _, b in raw_reads:
        off += len(b)
        if hdr < off < len(served) - 8:
            ok = gzip_boundary(served, hdr, decoded, off) is not None
            report['gzip read boundary at flush'] += ok
            report['gzip read boundary at flush but missed'] += ok and off not in fs
            report['gzip read boundary mid-block'] += not ok
    return flushes


if __name__ == '__main__':
    main(sys.argv[1:])
