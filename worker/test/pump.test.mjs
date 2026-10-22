import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { brotliDecompressSync } from 'node:zlib';
import { pump } from '../src/pump.js';

test('disconnect cancels a stalled origin and releases the slot', { timeout: 2000 }, async () => {
  let cancelled = 0;
  const released = [];
  const body = new ReadableStream({ cancel() { cancelled++; } });
  const { readable, writable } = new TransformStream();
  const x = {
    slot_in() { return 0; },
    slot_push() { return 0; },
    slot_release(slot) { released.push(slot); },
  };
  const pumping = pump(body, writable.getWriter(), x, 3);
  await readable.cancel('client disconnected');
  await pumping;
  assert.equal(cancelled, 1);
  assert.deepEqual(released, [3]);
  assert.equal(body.locked, false);
});

test('a lookahead error is handled while downstream is blocked', { timeout: 2000 }, async () => {
  let controller;
  let released = 0;
  const body = new ReadableStream({ start(c) { controller = c; } });
  const { readable, writable } = new TransformStream();
  const x = {
    memory: new WebAssembly.Memory({ initial: 1 }),
    slot_in() { return 16; },
    slot_push() { return 1; },
    slot_out() { return 16; },
    slot_release() { released++; },
  };
  const pumping = pump(body, writable.getWriter(), x, 0);
  controller.enqueue(new Uint8Array([42]));
  await new Promise((r) => setTimeout(r, 0));
  controller.error(new Error('origin disconnected'));
  await new Promise((r) => setTimeout(r, 0));
  await readable.cancel('client disconnected');
  await pumping;
  assert.equal(released, 1);
});

test('streamed wasm output round-trips through the independent Node decoder', async () => {
  const wasm = readFileSync(new URL('../../target/wasm32-unknown-unknown/wasm/chunky_wasm.wasm', import.meta.url));
  const x = new WebAssembly.Instance(new WebAssembly.Module(wasm), {}).exports;
  const slot = x.slot_admit(2 | (22 << 8));
  assert.ok(slot >= 0);
  const input = Buffer.from('<p>public template with a changing value</p>\n'.repeat(1000));
  const body = new ReadableStream({
    start(c) {
      c.enqueue(input.subarray(0, 7000));
      c.enqueue(input.subarray(7000));
      c.close();
    },
  });
  const { readable, writable } = new TransformStream();
  const output = new Response(readable).arrayBuffer();
  await pump(body, writable.getWriter(), x, slot);
  assert.deepEqual(brotliDecompressSync(Buffer.from(await output)), input);
  assert.equal(x.slot_push(slot, 0, 0), -1);
});

test('a null input buffer errors the body without writing to address 0', { timeout: 2000 }, async () => {
  const memory = new WebAssembly.Memory({ initial: 1 });
  new Uint8Array(memory.buffer).fill(7, 0, 8);
  let pushed = 0;
  let released = 0;
  const body = new ReadableStream({
    start(c) {
      c.enqueue(new Uint8Array([1, 2, 3]));
      c.close();
    },
  });
  const { readable, writable } = new TransformStream();
  const x = {
    memory,
    slot_in() { return 0; },
    slot_push() { pushed++; return 0; },
    slot_out() { return 0; },
    slot_release() { released++; },
  };
  const output = new Response(readable).arrayBuffer();
  await pump(body, writable.getWriter(), x, 0);
  await assert.rejects(output);
  assert.deepEqual([...new Uint8Array(memory.buffer, 0, 8)], [7, 7, 7, 7, 7, 7, 7, 7]);
  assert.equal(pushed, 0);
  assert.equal(released, 1);
});
