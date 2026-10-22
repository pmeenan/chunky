//! In-process FFI to the pinned C google/brotli release (D-023).
//!
//! Two roles: the CPU baseline encoder for the reference quality curve
//! (D-016), driven with the same call/flush sequence as the Rust paths, and
//! the independent reference decoder every output path round-trips through
//! (D-012). Allocations go through counting hooks so baselines can report
//! per-stream and shared (prepared dictionary) memory (D-025).

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::ptr::NonNull;

/// The vendored release tag and commit (third_party/google-brotli).
pub const C_BROTLI_TAG: &str = "v1.2.1";
pub const C_BROTLI_COMMIT: &str = "db24a98aed38a16de78cc05ca15cdd8dc2a0d92d";
/// Compiler and flags the vendored sources were built with.
pub const C_COMPILER: &str = env!("BROTLI_C_COMPILER");
pub const C_FLAGS: &str = env!("BROTLI_C_FLAGS");

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type AllocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
    pub type FreeFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

    #[repr(C)]
    pub struct EncoderState {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct PreparedDictionary {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct DecoderState {
        _p: [u8; 0],
    }

    unsafe extern "C" {
        pub fn BrotliEncoderCreateInstance(
            alloc: Option<AllocFn>,
            free: Option<FreeFn>,
            opaque: *mut c_void,
        ) -> *mut EncoderState;
        pub fn BrotliEncoderSetParameter(s: *mut EncoderState, p: c_int, v: u32) -> c_int;
        pub fn BrotliEncoderDestroyInstance(s: *mut EncoderState);
        pub fn BrotliEncoderPrepareDictionary(
            kind: c_int,
            size: usize,
            data: *const u8,
            quality: c_int,
            alloc: Option<AllocFn>,
            free: Option<FreeFn>,
            opaque: *mut c_void,
        ) -> *mut PreparedDictionary;
        pub fn BrotliEncoderDestroyPreparedDictionary(d: *mut PreparedDictionary);
        pub fn BrotliEncoderAttachPreparedDictionary(
            s: *mut EncoderState,
            d: *const PreparedDictionary,
        ) -> c_int;
        pub fn BrotliEncoderCompressStream(
            s: *mut EncoderState,
            op: c_int,
            avail_in: *mut usize,
            next_in: *mut *const u8,
            avail_out: *mut usize,
            next_out: *mut *mut u8,
            total_out: *mut usize,
        ) -> c_int;
        pub fn BrotliEncoderIsFinished(s: *mut EncoderState) -> c_int;
        pub fn BrotliEncoderHasMoreOutput(s: *mut EncoderState) -> c_int;
        pub fn BrotliEncoderVersion() -> u32;

        pub fn BrotliDecoderCreateInstance(
            alloc: Option<AllocFn>,
            free: Option<FreeFn>,
            opaque: *mut c_void,
        ) -> *mut DecoderState;
        pub fn BrotliDecoderAttachDictionary(
            s: *mut DecoderState,
            kind: c_int,
            size: usize,
            data: *const u8,
        ) -> c_int;
        pub fn BrotliDecoderDecompressStream(
            s: *mut DecoderState,
            avail_in: *mut usize,
            next_in: *mut *const u8,
            avail_out: *mut usize,
            next_out: *mut *mut u8,
            total_out: *mut usize,
        ) -> c_int;
        pub fn BrotliDecoderDestroyInstance(s: *mut DecoderState);
        pub fn BrotliDecoderGetErrorCode(s: *const DecoderState) -> c_int;
        pub fn BrotliDecoderErrorString(c: c_int) -> *const c_char;
        pub fn BrotliDecoderVersion() -> u32;
    }
}

const PARAM_MODE: c_int = 0;
const PARAM_QUALITY: c_int = 1;
const PARAM_LGWIN: c_int = 2;
const PARAM_LGBLOCK: c_int = 3;
const PARAM_DISABLE_LITERAL_CONTEXT_MODELING: c_int = 4;
const PARAM_SIZE_HINT: c_int = 5;
const SHARED_DICTIONARY_RAW: c_int = 0;
const DECODER_RESULT_SUCCESS: c_int = 1;
const DECODER_RESULT_NEEDS_MORE_INPUT: c_int = 2;
const DECODER_RESULT_NEEDS_MORE_OUTPUT: c_int = 3;

fn version_string(v: u32) -> String {
    format!("{}.{}.{}", v >> 24, (v >> 12) & 0xfff, v & 0xfff)
}

/// Runtime-reported encoder library version, e.g. "1.2.1".
pub fn encoder_version() -> String {
    // SAFETY: takes no arguments and only returns a constant.
    version_string(unsafe { ffi::BrotliEncoderVersion() })
}

/// Runtime-reported decoder library version.
pub fn decoder_version() -> String {
    // SAFETY: takes no arguments and only returns a constant.
    version_string(unsafe { ffi::BrotliDecoderVersion() })
}

/// Allocation counters shared by one C instance through its opaque pointer.
#[derive(Default, Debug)]
pub struct MemCounter {
    current: Cell<usize>,
    peak: Cell<usize>,
    allocs: Cell<usize>,
}

impl MemCounter {
    /// Bytes currently allocated (excluding the counter's own headers).
    pub fn current(&self) -> usize {
        self.current.get()
    }
    /// High-water mark of allocated bytes.
    pub fn peak(&self) -> usize {
        self.peak.get()
    }
    /// Number of allocation calls.
    pub fn allocs(&self) -> usize {
        self.allocs.get()
    }
}

// Every block carries a 16-byte header holding its size, so the free hook can
// account for it and rebuild the layout.
const HDR: usize = 16;

/// A heap `MemCounter` owned through a raw pointer, so the pointer handed to
/// C as `opaque` stays valid when the owning struct moves (moving a `Box`
/// would retag it and invalidate pointers derived from it).
struct Counter(NonNull<MemCounter>);

impl Counter {
    fn new() -> Self {
        Self(NonNull::from(Box::leak(Box::<MemCounter>::default())))
    }

    fn opaque(&self) -> *mut c_void {
        self.0.as_ptr().cast()
    }

    fn get(&self) -> &MemCounter {
        // SAFETY: the pointer came from Box::leak and is freed only in Drop;
        // C reads it only through shared references (Cell fields).
        unsafe { self.0.as_ref() }
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        // SAFETY: leaked in new() and reclaimed exactly once. Owners declare
        // this field after the C handle, whose Drop runs first, so C no longer
        // holds the pointer.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

unsafe extern "C" fn counting_alloc(opaque: *mut c_void, size: usize) -> *mut c_void {
    let Some(total) = size.checked_add(HDR) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = std::alloc::Layout::from_size_align(total, HDR) else {
        return std::ptr::null_mut();
    };
    // SAFETY: layout has a nonzero size (at least HDR).
    let p = unsafe { std::alloc::alloc(layout) };
    if p.is_null() {
        return p.cast();
    }
    // SAFETY: p is valid for HDR + size bytes and 16-aligned; opaque is the
    // owner's Counter, which outlives the C instance.
    unsafe {
        p.cast::<usize>().write(size);
        let m = &*(opaque as *const MemCounter);
        m.current.set(m.current.get() + size);
        m.peak.set(m.peak.get().max(m.current.get()));
        m.allocs.set(m.allocs.get() + 1);
        p.add(HDR).cast()
    }
}

unsafe extern "C" fn counting_free(opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    // SAFETY: address came from counting_alloc, so the header sits HDR bytes
    // before it and records the size used to build the original layout.
    unsafe {
        let p = address.cast::<u8>().sub(HDR);
        let size = p.cast::<usize>().read();
        let m = &*(opaque as *const MemCounter);
        m.current.set(m.current.get() - size);
        std::alloc::dealloc(
            p,
            std::alloc::Layout::from_size_align_unchecked(size + HDR, HDR),
        );
    }
}

/// Encoder parameters, mapped one-to-one onto C `BrotliEncoderSetParameter`.
#[derive(Clone, Debug)]
pub struct EncoderParams {
    pub quality: u32,
    pub lgwin: u32,
    /// 0 generic, 1 text, 2 font.
    pub mode: u32,
    pub lgblock: Option<u32>,
    pub disable_literal_context_modeling: bool,
    pub size_hint: Option<u32>,
}

impl EncoderParams {
    pub fn new(quality: u32, lgwin: u32) -> Self {
        Self {
            quality,
            lgwin,
            mode: 0,
            lgblock: None,
            disable_literal_context_modeling: false,
            size_hint: None,
        }
    }
}

/// Stream operation, as in `BrotliEncoderOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Process = 0,
    Flush = 1,
    Finish = 2,
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A raw (RFC 9841 Shared Brotli raw) dictionary prepared once and shared by
/// any number of encoders. C keeps a pointer to `data`, so it is borrowed.
pub struct PreparedDictionary<'d> {
    ptr: NonNull<ffi::PreparedDictionary>,
    mem: Counter,
    _data: PhantomData<&'d [u8]>,
}

impl<'d> PreparedDictionary<'d> {
    pub fn new(data: &'d [u8], quality: u32) -> Result<Self, Error> {
        let mem = Counter::new();
        // SAFETY: data outlives the returned value ('d); the hooks and the
        // counter outlive the C object, which Drop destroys first.
        let ptr = unsafe {
            ffi::BrotliEncoderPrepareDictionary(
                SHARED_DICTIONARY_RAW,
                data.len(),
                data.as_ptr(),
                quality as c_int,
                Some(counting_alloc),
                Some(counting_free),
                mem.opaque(),
            )
        };
        let ptr = NonNull::new(ptr).ok_or_else(|| Error("prepare dictionary failed".into()))?;
        Ok(Self {
            ptr,
            mem,
            _data: PhantomData,
        })
    }

    /// Memory the prepared index holds (excluding the borrowed data).
    pub fn mem(&self) -> &MemCounter {
        self.mem.get()
    }
}

impl Drop for PreparedDictionary<'_> {
    fn drop(&mut self) {
        // SAFETY: ptr came from BrotliEncoderPrepareDictionary and is freed once.
        unsafe { ffi::BrotliEncoderDestroyPreparedDictionary(self.ptr.as_ptr()) }
    }
}

/// One C encoder stream.
pub struct Encoder<'d> {
    state: NonNull<ffi::EncoderState>,
    mem: Counter,
    _dict: PhantomData<&'d PreparedDictionary<'d>>,
}

impl<'d> Encoder<'d> {
    pub fn new(p: &EncoderParams) -> Result<Self, Error> {
        let mem = Counter::new();
        // SAFETY: the hooks match the C signatures and the counter outlives
        // the instance, which Drop destroys.
        let state = unsafe {
            ffi::BrotliEncoderCreateInstance(
                Some(counting_alloc),
                Some(counting_free),
                mem.opaque(),
            )
        };
        let state = NonNull::new(state).ok_or_else(|| Error("create encoder failed".into()))?;
        let e = Self {
            state,
            mem,
            _dict: PhantomData,
        };
        e.set(PARAM_QUALITY, p.quality)?;
        e.set(PARAM_LGWIN, p.lgwin)?;
        e.set(PARAM_MODE, p.mode)?;
        if let Some(b) = p.lgblock {
            e.set(PARAM_LGBLOCK, b)?;
        }
        if p.disable_literal_context_modeling {
            e.set(PARAM_DISABLE_LITERAL_CONTEXT_MODELING, 1)?;
        }
        if let Some(h) = p.size_hint {
            e.set(PARAM_SIZE_HINT, h)?;
        }
        Ok(e)
    }

    fn set(&self, param: c_int, v: u32) -> Result<(), Error> {
        // SAFETY: state is a live encoder instance.
        if unsafe { ffi::BrotliEncoderSetParameter(self.state.as_ptr(), param, v) } == 0 {
            return Err(Error(format!("set parameter {param}={v} rejected")));
        }
        Ok(())
    }

    /// Attach a prepared dictionary; must happen before the first byte.
    pub fn attach(&mut self, d: &'d PreparedDictionary<'d>) -> Result<(), Error> {
        // SAFETY: both pointers are live; the borrow keeps d alive as long as self.
        if unsafe {
            ffi::BrotliEncoderAttachPreparedDictionary(self.state.as_ptr(), d.ptr.as_ptr())
        } == 0
        {
            return Err(Error("attach prepared dictionary failed".into()));
        }
        Ok(())
    }

    /// Feed `input` with `op` and append everything the encoder emits to
    /// `out`. Flush and Finish run until the operation completes.
    pub fn push(&mut self, input: &[u8], op: Op, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut avail_in = input.len();
        let mut next_in = input.as_ptr();
        loop {
            out.reserve(1 << 16);
            let spare = out.spare_capacity_mut();
            let cap = spare.len();
            let mut avail_out = cap;
            let mut next_out = spare.as_mut_ptr().cast::<u8>();
            // SAFETY: next_in/avail_in describe the unread tail of `input`;
            // next_out/avail_out describe `out`'s spare capacity.
            let ok = unsafe {
                ffi::BrotliEncoderCompressStream(
                    self.state.as_ptr(),
                    op as c_int,
                    &mut avail_in,
                    &mut next_in,
                    &mut avail_out,
                    &mut next_out,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(Error("BrotliEncoderCompressStream failed".into()));
            }
            let len = out.len();
            // SAFETY: the encoder initialized cap - avail_out bytes of spare capacity.
            unsafe { out.set_len(len + cap - avail_out) };
            // SAFETY: state is a live encoder instance.
            let more = unsafe { ffi::BrotliEncoderHasMoreOutput(self.state.as_ptr()) } != 0;
            if avail_in == 0 && !more {
                if op == Op::Finish
                    // SAFETY: state is a live encoder instance.
                    && unsafe { ffi::BrotliEncoderIsFinished(self.state.as_ptr()) } == 0
                {
                    continue;
                }
                return Ok(());
            }
        }
    }

    /// Memory this stream's instance holds (excluding shared dictionaries).
    pub fn mem(&self) -> &MemCounter {
        self.mem.get()
    }
}

impl Drop for Encoder<'_> {
    fn drop(&mut self) {
        // SAFETY: state came from BrotliEncoderCreateInstance and is freed once.
        unsafe { ffi::BrotliEncoderDestroyInstance(self.state.as_ptr()) }
    }
}

/// Decode a complete brotli stream with the C reference decoder, optionally
/// with a raw shared dictionary attached. Fails unless the stream ends with
/// SUCCESS and every input byte is consumed.
pub fn decode(stream: &[u8], dict: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    // SAFETY: default allocators (null hooks).
    let s = unsafe { ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut()) };
    let s = NonNull::new(s).ok_or_else(|| Error("create decoder failed".into()))?;
    struct Guard(NonNull<ffi::DecoderState>);
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: created above, destroyed once.
            unsafe { ffi::BrotliDecoderDestroyInstance(self.0.as_ptr()) }
        }
    }
    let g = Guard(s);
    if let Some(d) = dict {
        // SAFETY: d outlives the decoder, which the guard destroys at return.
        if unsafe {
            ffi::BrotliDecoderAttachDictionary(
                g.0.as_ptr(),
                SHARED_DICTIONARY_RAW,
                d.len(),
                d.as_ptr(),
            )
        } == 0
        {
            return Err(Error("attach dictionary failed".into()));
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(stream.len() * 4 + 1024);
    let mut avail_in = stream.len();
    let mut next_in = stream.as_ptr();
    loop {
        out.reserve(1 << 16);
        let spare = out.spare_capacity_mut();
        let cap = spare.len();
        let mut avail_out = cap;
        let mut next_out = spare.as_mut_ptr().cast::<u8>();
        // SAFETY: in/out pointers describe live buffers as in Encoder::push.
        let r = unsafe {
            ffi::BrotliDecoderDecompressStream(
                g.0.as_ptr(),
                &mut avail_in,
                &mut next_in,
                &mut avail_out,
                &mut next_out,
                std::ptr::null_mut(),
            )
        };
        let len = out.len();
        // SAFETY: the decoder initialized cap - avail_out bytes.
        unsafe { out.set_len(len + cap - avail_out) };
        match r {
            DECODER_RESULT_NEEDS_MORE_OUTPUT => continue,
            DECODER_RESULT_SUCCESS if avail_in == 0 => return Ok(out),
            DECODER_RESULT_SUCCESS => {
                return Err(Error(format!("{avail_in} trailing bytes after stream end")));
            }
            DECODER_RESULT_NEEDS_MORE_INPUT => return Err(Error("truncated stream".into())),
            _ => {
                // SAFETY: state is live; the error string is a static C string.
                let msg = unsafe {
                    let code = ffi::BrotliDecoderGetErrorCode(g.0.as_ptr());
                    CStr::from_ptr(ffi::BrotliDecoderErrorString(code) as *const c_char)
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(Error(format!("decode error: {msg}")));
            }
        }
    }
}

/// One-shot helper: compress `input` with the given flush schedule
/// (ascending offsets into `input`; flush after each) and finish.
pub fn compress_with_flushes(
    p: &EncoderParams,
    dict: Option<&PreparedDictionary<'_>>,
    input: &[u8],
    flushes: &[usize],
) -> Result<Vec<u8>, Error> {
    let mut e = Encoder::new(p)?;
    if let Some(d) = dict {
        e.attach(d)?;
    }
    let mut out = Vec::new();
    let mut pos = 0;
    for &f in flushes {
        let f = f.min(input.len());
        if f > pos {
            e.push(&input[pos..f], Op::Flush, &mut out)?;
            pos = f;
        }
    }
    e.push(&input[pos..], Op::Finish, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..2000 {
            v.extend_from_slice(
                format!("<li class=\"item\" data-id=\"{i}\">row {}</li>\n", i * 7).as_bytes(),
            );
        }
        v
    }

    #[test]
    fn versions_match_the_pin() {
        assert_eq!(encoder_version(), C_BROTLI_TAG.trim_start_matches('v'));
        assert_eq!(decoder_version(), C_BROTLI_TAG.trim_start_matches('v'));
    }

    #[test]
    fn round_trip_all_qualities_with_flushes() {
        let input = sample();
        for q in 0..=11 {
            let out =
                compress_with_flushes(&EncoderParams::new(q, 22), None, &input, &[15, 4096, 30000])
                    .unwrap();
            assert_eq!(decode(&out, None).unwrap(), input, "q{q}");
        }
    }

    #[test]
    fn dictionary_round_trip_and_memory_counting() {
        let input = sample();
        let dict = input[..20000].to_vec();
        let pd = PreparedDictionary::new(&dict, 11).unwrap();
        assert!(pd.mem().peak() > 0);
        let out =
            compress_with_flushes(&EncoderParams::new(5, 22), Some(&pd), &input, &[]).unwrap();
        assert_eq!(decode(&out, Some(&dict)).unwrap(), input);
        assert!(decode(&out, None).is_err());
    }

    #[test]
    fn rejects_truncated_and_trailing() {
        let input = sample();
        let out = compress_with_flushes(&EncoderParams::new(5, 22), None, &input, &[]).unwrap();
        assert!(decode(&out[..out.len() - 1], None).is_err());
        let mut extra = out.clone();
        extra.push(0);
        assert!(decode(&extra, None).is_err());
    }
}
