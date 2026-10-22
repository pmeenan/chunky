"""chunky M0 extraction spike (D-018).

Turns a Chrome netlog (capture mode Everything) into per-response manifests for
www.google.com/search documents plus the Use-As-Dictionary dictionaries they
use. Cookie and similar header values are redacted. Each body is decoded
independently with the C decoder (libbrotlidec, with the dictionary attached
for dcb) and checked against the netlog's decoded bytes. Flush boundaries come
from byte-aligned meta-block ends in the served stream; every one is validated
with the C decoder.

Build the scanner first (it is standalone until the M1 workspace exists):
  hostlock shared -- cargo build --release --manifest-path tools/netlog/flushscan/Cargo.toml

Approved sources: www.google.com/search and Etsy listing, category and shop
pages (D-018, D-022). Other documents in the netlog are skipped.

usage: python3 -I tools/netlog/extract.py <netlog.json> <out_dir> <flushscan_binary>
e.g.   python3 -I tools/netlog/extract.py data/netlogs/google-search.json \
           data/corpus/sessions/2026-10-21 tools/netlog/flushscan/target/release/flushscan

Needs the system libbrotlidec (C brotli >= 1.1.0, for dictionary attach).
Raw netlogs hold credential-equivalent data: keep them and all output under
data/ (git-ignored, D-007/D-018).
"""
import base64, collections, ctypes, datetime, hashlib, json, os, re, subprocess, sys, tempfile, urllib.parse, zlib

REDACT = {'cookie', 'set-cookie', 'x-client-data', 'authorization', 'proxy-authorization'}
DCB_MAGIC = b'\xffDCB'

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


def headers(lst):
    out = []
    for h in lst:
        if not isinstance(h, str) or ': ' not in h:
            continue
        k, v = h.split(': ', 1)
        k = k.lower()
        out.append([k, '<redacted %d bytes>' % len(v) if k in REDACT else v])
    return out


def first(hs, k):
    return next((v for kk, v in hs if kk == k), None)


def main(netlog, out_dir, flushscan):
    log = json.load(open(netlog))
    c = log['constants']
    ev = {v: k for k, v in c['logEventTypes'].items()}
    st = {v: k for k, v in c['logSourceType'].items()}
    tick0 = int(c['timeTickOffset'])
    reqs = collections.defaultdict(list)
    for e in log['events']:
        if st.get(e['source']['type']) == 'URL_REQUEST':
            reqs[e['source']['id']].append(e)
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
                       'captured_utc': datetime.datetime.fromtimestamp((tick0 + r['start']) / 1000, datetime.timezone.utc).isoformat()},
                      open(os.path.join(out_dir, 'dicts', h.hex() + '.json'), 'w'), indent=1)
    manifest = open(os.path.join(out_dir, 'manifest.jsonl'), 'w')
    report = collections.Counter()
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
            flushes = recover_flushes(stream, dictionary, decoded, hdr, r['raw'], flushscan, report)
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
        manifest.write(json.dumps({
            'manifest_version': 1, 'id': r['id'],
            'captured_utc': datetime.datetime.fromtimestamp((tick0 + r['start']) / 1000, datetime.timezone.utc).isoformat(),
            'site': site, 'url': r['url'], 'query': q.get('q', [''])[0] if site == 'google' else '', 'route': route + device,
            'device': (first(r['req'], 'sec-ch-ua-model') or first(r['req'], 'sec-ch-ua-platform') or '').strip('"'),
            'protocol': r['proto'], 'status_line': r['status_line'], 'request_headers': r['req'], 'response_headers': r['resp'],
            'served_encoding': ce, 'decode_verified_by': verified, 'dictionary_sha256': dict_sfv,
            'served_bytes': len(served), 'decoded_bytes': len(decoded),
            'body_sha256': body_sha, 'served_file': 'served/%d.%s' % (r['id'], ce or 'identity'),
            'transport_reads': [[t - r['start'], len(b)] for t, b in r['raw']],
            'flushes': [{'decoded_offset': d, 'stream_offset': s, 'arrival_ms': arrival(s)} for s, d in flushes],
            'flags': flags}) + '\n')
        label = q.get('q', [''])[0] if site == 'google' else u.path
        print('%-34s %-4s %4s %8d -> %8d  flushes=%3d  reads=%3d  last_read=%5dms %s' % (
            label[:34], ce, r['proto'], len(served), len(decoded), len(flushes), len(r['raw']), r['raw'][-1][0] - r['start'] if r['raw'] else 0, flags or ''))
    print(dict(report))


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


def recover_flushes(stream, dictionary, decoded, hdr, raw_reads, flushscan, report):
    """Byte-aligned meta-block ends proposed by flushscan, each validated with the C decoder."""
    with tempfile.TemporaryDirectory() as td:
        sp, dp, op = (os.path.join(td, x) for x in ('s', 'd', 'o'))
        open(sp, 'wb').write(stream)
        open(dp, 'wb').write(dictionary)
        res = subprocess.run([flushscan, sp, dp if dictionary else '-', op], capture_output=True, text=True, check=True)
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
    main(*sys.argv[1:4])
