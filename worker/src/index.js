// chunky Worker (M1): a thin JS shell around raw wasm exports (D-021) that
// br-encodes origin bodies with the rust-brotli reference encoder through
// D-024's slot interface. M1 uses it to measure reference-encoder CPU and
// memory at the edge; admission accounting, fixed slot reservations,
// telemetry and the full response contract land in M6.
//
// Flush policy: coalesce reads that are already available (RE-007). After
// feeding a read, flush only if the next read is not ready by the next
// macrotask.

import wasmModule from '../dist/chunky_wasm.wasm';
import { pump } from './pump.js';

let wasm; // one instance per isolate (D-024, D-025)
function exports() {
  wasm ??= new WebAssembly.Instance(wasmModule, {}).exports;
  return wasm;
}

const COMPRESSIBLE = /^(text\/|application\/(json|javascript|xml|xhtml\+xml|ld\+json|manifest\+json)|image\/svg\+xml)/i;

// q-value of `coding` in an Accept-Encoding list (0 if absent).
function acceptQ(header, coding) {
  for (const part of (header || '').split(',')) {
    const [name, ...params] = part.trim().split(';');
    if (name.trim().toLowerCase() !== coding) continue;
    const q = params.map((p) => p.trim()).find((p) => /^q=/i.test(p));
    return q ? Number(q.slice(2)) || 0 : 1;
  }
  return 0;
}

function shouldEncode(request, response) {
  if (request.method !== 'GET' || response.status !== 200 || !response.body) return false;
  if (response.headers.get('content-encoding')) return false;
  if (/no-transform/i.test(response.headers.get('cache-control') || '')) return false;
  // A client asking intermediaries not to transform content (RFC 9111 5.2.1.6).
  if (/no-transform/i.test(request.headers.get('cache-control') || '')) return false;
  if (!COMPRESSIBLE.test(response.headers.get('content-type') || '')) return false;
  // Negotiate from the client's real header, never the normalized one (RE-005).
  const ae = request.cf?.clientAcceptEncoding ?? request.headers.get('accept-encoding');
  return acceptQ(ae, 'br') > 0;
}

function encodedHeaders(h) {
  const out = new Headers(h);
  out.set('content-encoding', 'br');
  out.delete('content-length');
  const vary = out.get('vary');
  if (!vary) out.set('vary', 'Accept-Encoding');
  else if (!/(^|,)\s*(accept-encoding|\*)\s*(,|$)/i.test(vary)) out.set('vary', `${vary}, Accept-Encoding`);
  const cc = out.get('cache-control');
  out.set('cache-control', cc ? `${cc}, no-transform` : 'no-transform'); // RE-005
  const etag = out.get('etag');
  if (etag && !etag.startsWith('W/')) out.set('etag', `W/${etag}`);
  return out;
}

export default {
  async fetch(request, env, ctx) {
    let upstream = request;
    if (env.CHUNKY_ORIGIN) {
      const u = new URL(request.url);
      const o = new URL(env.CHUNKY_ORIGIN);
      u.protocol = o.protocol;
      u.host = o.host;
      upstream = new Request(u, request);
    }
    const response = await fetch(upstream);
    if (!shouldEncode(request, response)) return response;
    const x = exports();
    const q = Number(env.CHUNKY_QUALITY ?? 2);
    const w = Number(env.CHUNKY_LGWIN ?? 22);
    const slot = x.slot_admit(q | (w << 8));
    // No slot before commitment: let the edge compress it (D-024).
    if (slot < 0) return response;
    const { readable, writable } = new TransformStream();
    ctx.waitUntil(pump(response.body, writable.getWriter(), x, slot));
    return new Response(readable, {
      status: response.status,
      statusText: response.statusText,
      headers: encodedHeaders(response.headers),
      encodeBody: 'manual',
    });
  },
};
