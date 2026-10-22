//! Raw wasm exports for the Worker's thin JS shell (D-021), using D-024's
//! slot interface. M1 drives the reference encoder only, so the M1 Worker can
//! measure reference-encoder CPU and memory in V8; fixed slot reservations
//! and admission land in M6.
//!
//! JS protocol per stream: `slot_admit` → repeat { write input at
//! `slot_in(slot, len)`, call `slot_push(slot, len, op)`, read the returned
//! byte count at `slot_out(slot)` } → `slot_release`. The output pointer is
//! valid until the next call into the module.

use std::cell::RefCell;

use chunky_core::reference::{Op, ReferenceEncoder, ReferenceParams};

const SLOTS: usize = 8;

struct Slot {
    enc: ReferenceEncoder,
    input: Vec<u8>,
    output: Vec<u8>,
}

thread_local! {
    static SLOT_TABLE: RefCell<[Option<Slot>; SLOTS]> = const { RefCell::new([const { None }; SLOTS]) };
}

/// Error codes returned (negated) by `slot_push`.
pub const ERR_BAD_SLOT: i32 = -1;
pub const ERR_BAD_OP: i32 = -2;
pub const ERR_ENCODE: i32 = -3;
pub const ERR_BAD_LEN: i32 = -4;

/// Reserve a slot. `options` packs quality (bits 0–7) and lgwin (bits 8–15).
/// Returns the slot index, or -1 if none is free or the options are invalid.
#[unsafe(no_mangle)]
pub extern "C" fn slot_admit(options: u32) -> i32 {
    let p = ReferenceParams::new(options & 0xff, (options >> 8) & 0xff);
    if p.quality > 11 || !(10..=24).contains(&p.lgwin) {
        return -1;
    }
    SLOT_TABLE.with_borrow_mut(|t| {
        let Some(i) = t.iter().position(Option::is_none) else {
            return -1;
        };
        match ReferenceEncoder::new(&p) {
            Ok(enc) => {
                t[i] = Some(Slot {
                    enc,
                    input: Vec::new(),
                    output: Vec::new(),
                });
                i as i32
            }
            Err(_) => -1,
        }
    })
}

/// Pointer to the slot's input buffer, holding at least `len` bytes; 0 on a
/// bad slot.
#[unsafe(no_mangle)]
pub extern "C" fn slot_in(slot: i32, len: u32) -> *mut u8 {
    SLOT_TABLE.with_borrow_mut(|t| match t.get_mut(slot as usize) {
        Some(Some(s)) => {
            // Grow only: the caller overwrites the bytes, so re-zeroing a
            // shrunk buffer on every small read or flush is wasted work.
            if len as usize > s.input.len() {
                s.input.resize(len as usize, 0);
            }
            s.input.as_mut_ptr()
        }
        _ => std::ptr::null_mut(),
    })
}

/// Encode `len` input bytes with `op` (0 feed, 1 flush, 2 finish). Returns
/// the number of output bytes at `slot_out`, or a negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn slot_push(slot: i32, len: u32, op: u32) -> i32 {
    let op = match op {
        0 => Op::Process,
        1 => Op::Flush,
        2 => Op::Finish,
        _ => return ERR_BAD_OP,
    };
    SLOT_TABLE.with_borrow_mut(|t| {
        let Some(Some(s)) = t.get_mut(slot as usize) else {
            return ERR_BAD_SLOT;
        };
        let len = len as usize;
        if len > s.input.len() {
            return ERR_BAD_LEN;
        }
        s.output.clear();
        match s.enc.push(&s.input[..len], op, &mut s.output) {
            Ok(()) => s.output.len() as i32,
            Err(_) => ERR_ENCODE,
        }
    })
}

/// Pointer to the slot's output from the last `slot_push`; 0 on a bad slot.
#[unsafe(no_mangle)]
pub extern "C" fn slot_out(slot: i32) -> *const u8 {
    SLOT_TABLE.with_borrow(|t| match t.get(slot as usize) {
        Some(Some(s)) => s.output.as_ptr(),
        _ => std::ptr::null(),
    })
}

/// Free the slot. Safe to call on any index, including on every error path.
#[unsafe(no_mangle)]
pub extern "C" fn slot_release(slot: i32) {
    SLOT_TABLE.with_borrow_mut(|t| {
        if let Some(s) = t.get_mut(slot as usize) {
            *s = None;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_protocol_round_trips() {
        let body: Vec<u8> = (0..20_000u32)
            .map(|i| b"<p>hello</p>"[(i % 12) as usize])
            .collect();
        let s = slot_admit(5 | (22 << 8));
        assert!(s >= 0);
        let mut out = Vec::new();
        for (chunk, op) in [(&body[..7000], 1), (&body[7000..], 2)] {
            let p = slot_in(s, chunk.len() as u32);
            // SAFETY: slot_in returned a buffer of at least chunk.len() bytes,
            // and nothing else touches the slot until slot_push.
            unsafe { std::ptr::copy_nonoverlapping(chunk.as_ptr(), p, chunk.len()) };
            let n = slot_push(s, chunk.len() as u32, op);
            assert!(n >= 0);
            // SAFETY: slot_out points at n initialized bytes until the next call.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(slot_out(s), n as usize) });
        }
        slot_release(s);
        assert_eq!(slot_push(s, 0, 0), ERR_BAD_SLOT);
        let mut dec = Vec::new();
        brotli_decompress(&out, &mut dec);
        assert_eq!(dec, body);
    }

    fn brotli_decompress(src: &[u8], dst: &mut Vec<u8>) {
        use std::io::Read;
        brotli_decompressor::Decompressor::new(src, 4096)
            .read_to_end(dst)
            .unwrap();
    }
}
