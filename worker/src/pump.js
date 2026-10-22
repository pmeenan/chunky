const OP_FEED = 0;
const OP_FLUSH = 1;
const OP_FINISH = 2;

// Pump origin reads through the encoder, honoring downstream backpressure.
// Errors after commitment error the body; the slot is released on every path.
export async function pump(body, writer, x, slot) {
  const reader = body.getReader();
  // A client can disconnect while the origin's next read is still pending.
  // Cancel that read so the pump can finish and release its encoder slot.
  writer.closed.catch((e) => reader.cancel(e).catch(() => {}));
  const read = () => {
    const pending = reader.read();
    // Observe lookahead errors immediately, including while a write is
    // blocked. Awaiting the original promise still propagates the error.
    pending.catch(() => {});
    return pending;
  };
  const push = async (chunk, op) => {
    const ptr = x.slot_in(slot, chunk.length);
    // 0 is a bad slot or a failed allocation: never write at address 0.
    if (chunk.length && !ptr) throw new Error('encoder input buffer unavailable');
    if (chunk.length) new Uint8Array(x.memory.buffer, ptr, chunk.length).set(chunk);
    const n = x.slot_push(slot, chunk.length, op);
    if (n < 0) throw new Error(`encoder error ${n}`);
    if (n > 0) {
      await writer.ready;
      await writer.write(new Uint8Array(x.memory.buffer, x.slot_out(slot), n).slice());
    }
  };
  // True if `p` settles before the next macrotask. The timer is cleared
  // either way, so a fast origin does not leave one timer per read pending.
  const settlesFirst = (p) => {
    let timer;
    const idle = new Promise((r) => {
      timer = setTimeout(() => r(false), 0);
    });
    return Promise.race([p.then(() => true, () => true), idle]).finally(() => clearTimeout(timer));
  };
  try {
    let pending = read();
    for (;;) {
      const r = await pending;
      if (r.done) break;
      pending = read();
      await push(r.value, OP_FEED);
      if (!(await settlesFirst(pending))) {
        await push(new Uint8Array(0), OP_FLUSH);
      }
    }
    await push(new Uint8Array(0), OP_FINISH);
    await writer.close();
  } catch (e) {
    reader.cancel(e).catch(() => {});
    writer.abort(e).catch(() => {});
  } finally {
    x.slot_release(slot);
    reader.releaseLock();
  }
}
