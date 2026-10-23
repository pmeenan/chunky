import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';

const root = new URL('../../', import.meta.url).pathname;
const wasmPath = join(root, 'target/wasm32-unknown-unknown/wasm/chunky_wasm.wasm');

const hex = (n) => BigInt.asUintN(64, n).toString(16).padStart(16, '0');

test('native and wasm identity digests match', async () => {
  assert.ok(
    existsSync(wasmPath),
    `${wasmPath} is missing; run the wasm build step first: ` +
      'cargo build --target wasm32-unknown-unknown --profile wasm -p chunky-wasm',
  );
  const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
  const wasm = hex(instance.exports.identity_digest());

  const r = spawnSync('cargo', ['run', '-q', '-p', 'chunky-cli', '--', 'identity-digest'], {
    cwd: root,
    encoding: 'utf8',
  });
  assert.equal(r.status, 0, `native identity-digest failed:\n${r.stderr}`);
  const native = r.stdout.trim();

  assert.match(native, /^[0-9a-f]{16}$/, `unexpected native output: ${r.stdout}`);
  assert.equal(wasm, native, `identity digest mismatch: wasm ${wasm} != native ${native}`);
});
