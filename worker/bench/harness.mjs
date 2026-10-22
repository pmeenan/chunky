// V8 harness for the wasm encoders (M1): rust-brotli via crates/chunky-wasm
// and the pinned C google/brotli via crates/brotli-c/wasm (wasi-sdk). Both
// expose the same slot exports, so they are driven with identical call/flush
// sequences (D-016, RE-002).
//
// identity: re-encode every (response, rust|c, quality, schedule) row of a
//   `chunky baseline` JSONL in wasm and compare SHA-256 with the native
//   output (RE-003: rust-brotli with portable-float must match).
//     node worker/bench/harness.mjs identity data/reports/m1/curve-w22.jsonl
// bench: V8 CPU per config, 1 warm-up + N timed passes, median and MAD, plus
//   the instance's wasm memory high-water per config (one fresh instance
//   per config). Run under the exclusive hold, pinned:
//     hostlock exclusive --label "chunky: wasm bench" -- taskset -c 2 \
//       node worker/bench/harness.mjs bench data/corpus/sessions/2026-10-2{1,2}
//   Options: --engines rust,c --qualities 1,2,5,9,11 --schedules buffered,served,reads
//            --reps 10 --sample-bytes 8388608 --lgwin 22 --out <json>
// Node timings are relative A/B numbers only (D-023); Worker CPU comes from
// production $workers.cpuTimeMs.

import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WASI } from 'node:wasi';
import { performance } from 'node:perf_hooks';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const WASM = {
  rust: join(root, 'target/wasm32-unknown-unknown/wasm/chunky_wasm.wasm'),
  c: join(root, 'target/wasm-c/brotli_c.wasm'),
};
const modules = {};

async function instance(engine) {
  modules[engine] ??= await WebAssembly.compile(readFileSync(WASM[engine]));
  if (engine === 'c') {
    const wasi = new WASI({ version: 'preview1', args: [], env: {}, returnOnExit: true });
    const inst = await WebAssembly.instantiate(modules[engine], wasi.getImportObject());
    wasi.initialize(inst);
    return inst.exports;
  }
  const inst = await WebAssembly.instantiate(modules[engine], {});
  return inst.exports;
}

// Encode one body along a schedule through the slot exports.
function encode(x, quality, lgwin, body, cuts, flush) {
  const slot = x.slot_admit(quality | (lgwin << 8));
  if (slot < 0) throw new Error('slot_admit failed');
  const parts = [];
  let total = 0;
  const push = (chunk, op) => {
    const p = x.slot_in(slot, chunk.length);
    if (chunk.length && !p) throw new Error('slot_in returned 0');
    if (chunk.length) new Uint8Array(x.memory.buffer, p, chunk.length).set(chunk);
    const n = x.slot_push(slot, chunk.length, op);
    if (n < 0) throw new Error(`slot_push ${n}`);
    if (n > 0) {
      parts.push(new Uint8Array(x.memory.buffer, x.slot_out(slot), n).slice());
      total += n;
    }
  };
  try {
    let pos = 0;
    for (const c of cuts) {
      if (c > pos && c < body.length) {
        push(body.subarray(pos, c), flush ? 1 : 0);
        pos = c;
      }
    }
    push(body.subarray(pos), 2);
  } finally {
    x.slot_release(slot);
  }
  const out = new Uint8Array(total);
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
}

// --- schedules: must match crates/chunky-cli/src/encoders.rs ---
const M64 = (1n << 64n) - 1n;
function syntheticReads(len, shaHex) {
  let s = BigInt('0x' + (shaHex.slice(0, 16) || '0'));
  const cuts = [];
  let pos = 0;
  for (;;) {
    s = (s + 0x9e3779b97f4a7c15n) & M64;
    let z = s;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & M64;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & M64;
    z = z ^ (z >> 31n);
    const n = z % 100n < 58n ? 1 + Number((z >> 8n) % 81n) : 4096;
    pos += n;
    if (pos >= len) return cuts;
    cuts.push(pos);
  }
}

function schedule(name, row) {
  const len = row.decoded_bytes;
  switch (name) {
    case 'buffered': return { cuts: [], flush: false };
    case 'served': return { cuts: row.flushes.filter((f) => f < len), flush: true };
    case 'reads': return { cuts: syntheticReads(len, row.body_sha256), flush: true };
    case 'reads-noflush': return { cuts: syntheticReads(len, row.body_sha256), flush: false };
    case 'every4k': {
      const cuts = [];
      for (let c = 4096; c < len; c += 4096) cuts.push(c);
      return { cuts, flush: true };
    }
    default: throw new Error(`unknown schedule ${name}`);
  }
}

// --- corpus ---
function loadDir(dir) {
  const group = dir.replace(/\/+$/, '').split('/').pop();
  return readFileSync(join(dir, 'manifest.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => {
    const v = JSON.parse(l);
    return {
      dir, group, id: String(v.id ?? ''), site: v.site, body_sha256: v.body_sha256,
      decoded_bytes: v.decoded_bytes, flags: v.flags ?? [],
      flushes: (v.flushes ?? []).map((f) => f.decoded_offset),
    };
  }).filter((r) => !r.flags.some((f) => ['interstitial', 'captcha', 'consent'].includes(f)));
}

function body(r) {
  const html = join(r.dir, 'bodies', r.body_sha256 + '.html');
  return new Uint8Array(readFileSync(existsSync(html) ? html : join(r.dir, 'bodies', r.body_sha256)));
}

const sha256 = (b) => createHash('sha256').update(b).digest('hex');

function opts(argv) {
  const o = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith('--')) o[argv[i].slice(2)] = argv[++i];
    else o._.push(argv[i]);
  }
  return o;
}

async function identity(o) {
  const want = readFileSync(o._[0], 'utf8').split('\n').filter(Boolean).map(JSON.parse)
    .filter((r) => !r.dict && !r.no_lcm && r.sha256 && (!o.engines || o.engines.split(',').includes(r.engine)));
  if (!want.length) throw new Error('no identity comparisons selected');
  const dirs = [...new Set(want.map((r) => r.group))];
  const corpusDirs = o.corpus ? o.corpus.split(',') : dirs.map((g) => join(root, 'data/corpus/sessions', g));
  const rows = new Map();
  for (const d of corpusDirs) for (const r of loadDir(d)) rows.set(`${r.group}#${r.id}`, r);
  const bodies = new Map();
  const x = {};
  for (const engine of new Set(want.map((r) => r.engine))) x[engine] = await instance(engine);
  const tally = {};
  const mismatches = [];
  for (const w of want) {
    const r = rows.get(`${w.group}#${w.id}`);
    if (!r) throw new Error(`row ${w.group}#${w.id} not in corpus`);
    if (!bodies.has(r.body_sha256)) bodies.set(r.body_sha256, body(r));
    const s = schedule(w.schedule, r);
    const out = encode(x[w.engine], w.q, w.lgwin, bodies.get(r.body_sha256), s.cuts, s.flush);
    const ok = out.length === w.size && sha256(out) === w.sha256;
    const k = `${w.engine} q${w.q}`;
    tally[k] ??= { same: 0, differ: 0 };
    tally[k][ok ? 'same' : 'differ']++;
    if (!ok) mismatches.push({ engine: w.engine, q: w.q, schedule: w.schedule, id: w.id, native: w.size, wasm: out.length });
  }
  console.log('| Engine, quality | Identical | Differ |\n| --- | --- | --- |');
  for (const [k, v] of Object.entries(tally)) console.log(`| ${k} | ${v.same} | ${v.differ} |`);
  if (mismatches.length) console.log('\nFirst mismatches:', JSON.stringify(mismatches.slice(0, 10)));
  if (o.out) writeFileSync(o.out, JSON.stringify({ tally, mismatches }, null, 1));
  process.exitCode = mismatches.length ? 1 : 0;
}

function medMad(v) {
  const s = [...v].sort((a, b) => a - b);
  const med = s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
  const d = v.map((x) => Math.abs(x - med)).sort((a, b) => a - b);
  const mad = d.length % 2 ? d[(d.length - 1) / 2] : (d[d.length / 2 - 1] + d[d.length / 2]) / 2;
  return [med, mad];
}

async function bench(o) {
  const engines = (o.engines ?? 'rust,c').split(',');
  const qs = (o.qualities ?? '1,2,5,9,11').split(',').map(Number);
  const scheds = (o.schedules ?? 'buffered,served,reads').split(',');
  const reps = Number(o.reps ?? 10);
  const lgwin = Number(o.lgwin ?? 22);
  const budget = Number(o['sample-bytes'] ?? 8 << 20);
  if (!Number.isInteger(reps) || reps < 1 || !Number.isFinite(budget) || budget <= 0) {
    throw new Error('--reps and --sample-bytes must be positive (--reps must be an integer)');
  }
  let rows = o._.flatMap(loadDir).sort((a, b) => a.body_sha256.localeCompare(b.body_sha256));
  rows = rows.filter((r, i) => i === 0 || r.body_sha256 !== rows[i - 1].body_sha256);
  rows = rows.filter((r) => r.decoded_bytes > 0);
  if (!rows.length) throw new Error('no eligible responses in the benchmark corpus');
  // Same even spread as `chunky bench`.
  const total = rows.reduce((a, r) => a + r.decoded_bytes, 0);
  if (total > budget) {
    const step = total / budget;
    let acc = 0;
    const spread = rows.filter(() => { acc += 1; if (acc >= step) { acc -= step; return true; } return false; });
    rows = spread.length ? spread : [rows[0]];
  }
  const bodies = rows.map(body);
  const bytes = bodies.reduce((a, b) => a + b.length, 0);
  console.log(`Node ${process.version} (V8 ${process.versions.v8}); ${rows.length} responses, ${bytes} decoded bytes; ` +
    `1 warm-up + ${reps} timed passes; lgwin ${lgwin}.\n`);
  console.log('| Engine | Quality | Schedule | Median ms | MAD ms | ms/MB | MB/s | wasm memory high-water |');
  console.log('| --- | --- | --- | --- | --- | --- | --- | --- |');
  const results = [];
  for (const e of engines) {
    for (const q of qs) {
      for (const sn of scheds) {
        const x = await instance(e);
        const ss = rows.map((r) => schedule(sn, r));
        const pass = () => {
          const t = performance.now();
          for (let i = 0; i < rows.length; i++) encode(x, q, lgwin, bodies[i], ss[i].cuts, ss[i].flush);
          return performance.now() - t;
        };
        pass();
        const times = Array.from({ length: reps }, pass);
        const [med, mad] = medMad(times);
        const msmb = med / (bytes / 1e6);
        const mem = x.memory.buffer.byteLength;
        console.log(`| ${e} | q${q} | ${sn} | ${med.toFixed(1)} | ${mad.toFixed(2)} | ${msmb.toFixed(2)} | ${(1e3 / msmb).toFixed(1)} | ${(mem / 2 ** 20).toFixed(1)} MiB |`);
        results.push({ engine: e, q, lgwin, schedule: sn, bytes, responses: rows.length, median_ms: med, mad_ms: mad, ms_per_mb: msmb, wasm_memory_bytes: mem, times_ms: times });
      }
    }
  }
  if (o.out) writeFileSync(o.out, JSON.stringify({ node: process.version, v8: process.versions.v8, results }, null, 1));
}

const [cmd, ...rest] = process.argv.slice(2);
const o = opts(rest);
if (cmd === 'identity') await identity(o);
else if (cmd === 'bench') await bench(o);
else {
  console.error('usage: harness.mjs identity <baseline.jsonl> | bench <manifest dirs...>');
  process.exitCode = 2;
}
