import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';

test('identity mismatches fail and a small benchmark budget still selects a body', () => {
  const dir = mkdtempSync(join(tmpdir(), 'chunky-identity-'));
  try {
    mkdirSync(join(dir, 'corpus', 'bodies'), { recursive: true });
    writeFileSync(join(dir, 'corpus', 'bodies', 'body.html'), 'synthetic public response');
    writeFileSync(join(dir, 'corpus', 'manifest.jsonl'), JSON.stringify({
      id: 1, body_sha256: 'body', decoded_bytes: 25, flushes: [],
    }) + '\n');
    writeFileSync(join(dir, 'baseline.jsonl'), JSON.stringify({
      group: 'corpus', id: 1, engine: 'rust', q: 2, lgwin: 22,
      schedule: 'buffered', sha256: 'deliberately incorrect', size: 1,
    }) + '\n');
    const r = spawnSync(process.execPath, [
      new URL('../bench/harness.mjs', import.meta.url).pathname,
      'identity', join(dir, 'baseline.jsonl'), '--corpus', join(dir, 'corpus'),
    ], { encoding: 'utf8' });
    assert.equal(r.status, 1, r.stdout + r.stderr);
    assert.match(r.stdout, /rust q2 \| 0 \| 1/);
    const bench = spawnSync(process.execPath, [
      new URL('../bench/harness.mjs', import.meta.url).pathname,
      'bench', join(dir, 'corpus'), '--engines', 'rust', '--qualities', '2',
      '--schedules', 'buffered', '--reps', '1', '--sample-bytes', '1',
    ], { encoding: 'utf8' });
    assert.equal(bench.status, 0, bench.stdout + bench.stderr);
    assert.match(bench.stdout, /1 responses, 25 decoded bytes/);
    assert.doesNotMatch(bench.stdout, /NaN|Infinity/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
