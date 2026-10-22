/* wasm32 driver for the pinned C google/brotli (D-023): the Worker CPU
 * baseline. Exposes the same slot protocol as crates/chunky-wasm so one JS
 * harness drives both encoders with identical call/flush sequences:
 *   slot_admit(options) -> slot | -1   options = quality | lgwin << 8
 *   slot_in(slot, len) -> ptr          input buffer of at least len bytes
 *   slot_push(slot, len, op) -> n | <0 op: 0 process, 1 flush, 2 finish
 *   slot_out(slot) -> ptr              n output bytes from the last push
 *   slot_release(slot)
 * Built as a WASI reactor by build.sh. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <brotli/encode.h>

#define SLOTS 8
#define EXPORT(name) __attribute__((export_name(#name)))

typedef struct {
  BrotliEncoderState* enc;
  uint8_t* in;
  size_t in_cap;
  uint8_t* out;
  size_t out_len;
  size_t out_cap;
} Slot;

static Slot slots[SLOTS];

EXPORT(slot_admit) int32_t slot_admit(uint32_t options) {
  uint32_t q = options & 0xff, w = (options >> 8) & 0xff;
  if (q > 11 || w < 10 || w > 24) return -1;
  for (int i = 0; i < SLOTS; i++) {
    if (slots[i].enc) continue;
    BrotliEncoderState* e = BrotliEncoderCreateInstance(NULL, NULL, NULL);
    if (!e) return -1;
    BrotliEncoderSetParameter(e, BROTLI_PARAM_QUALITY, q);
    BrotliEncoderSetParameter(e, BROTLI_PARAM_LGWIN, w);
    slots[i].enc = e;
    slots[i].out_len = 0;
    return i;
  }
  return -1;
}

static Slot* get(int32_t s) {
  return (s >= 0 && s < SLOTS && slots[s].enc) ? &slots[s] : NULL;
}

EXPORT(slot_in) uint8_t* slot_in(int32_t s, uint32_t len) {
  Slot* sl = get(s);
  if (!sl) return NULL;
  if (len > sl->in_cap) {
    uint8_t* p = realloc(sl->in, len);
    if (!p) return NULL;
    sl->in = p;
    sl->in_cap = len;
  }
  return sl->in;
}

static int grow_out(Slot* sl, size_t need) {
  if (need <= sl->out_cap) return 1;
  size_t cap = sl->out_cap ? sl->out_cap : 65536;
  while (cap < need) cap *= 2;
  uint8_t* p = realloc(sl->out, cap);
  if (!p) return 0;
  sl->out = p;
  sl->out_cap = cap;
  return 1;
}

EXPORT(slot_push) int32_t slot_push(int32_t s, uint32_t len, uint32_t op) {
  Slot* sl = get(s);
  if (!sl) return -1;
  if (op > 2) return -2;
  if (len > sl->in_cap) return -4;
  size_t avail_in = len;
  const uint8_t* next_in = sl->in;
  sl->out_len = 0;
  for (;;) {
    if (!grow_out(sl, sl->out_len + 65536)) return -3;
    size_t avail_out = sl->out_cap - sl->out_len;
    uint8_t* next_out = sl->out + sl->out_len;
    if (!BrotliEncoderCompressStream(sl->enc, (BrotliEncoderOperation)op, &avail_in,
                                     &next_in, &avail_out, &next_out, NULL))
      return -3;
    sl->out_len = (size_t)(next_out - sl->out);
    if (avail_in == 0 && !BrotliEncoderHasMoreOutput(sl->enc)) {
      if (op == 2 && !BrotliEncoderIsFinished(sl->enc)) continue;
      return (int32_t)sl->out_len;
    }
  }
}

EXPORT(slot_out) const uint8_t* slot_out(int32_t s) {
  Slot* sl = get(s);
  return sl ? sl->out : NULL;
}

EXPORT(slot_release) void slot_release(int32_t s) {
  Slot* sl = get(s);
  if (!sl) return;
  BrotliEncoderDestroyInstance(sl->enc);
  free(sl->in);
  free(sl->out);
  memset(sl, 0, sizeof(*sl));
}
