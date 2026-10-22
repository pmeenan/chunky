//! A counting global allocator, so memory baselines (D-025) can attribute
//! heap growth to one encoder instance on the measuring thread. Counters are
//! process-wide; measurements run single-threaded.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to System with the caller's arguments, so
// System's guarantees carry over; the counters do not affect allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; caller upholds GlobalAlloc's contract.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: as above.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            CURRENT.fetch_sub(l.size(), Relaxed);
            grow(new);
        }
        q
    }
}

fn grow(n: usize) {
    let c = CURRENT.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
}

/// Reset the high-water mark to the current allocation and return it.
pub fn reset_peak() -> usize {
    let c = CURRENT.load(Relaxed);
    PEAK.store(c, Relaxed);
    c
}

/// High-water mark since the last `reset_peak`.
pub fn peak() -> usize {
    PEAK.load(Relaxed)
}
