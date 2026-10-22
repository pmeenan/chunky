//! Find byte-aligned meta-block boundaries in a brotli stream by feeding the
//! decoder one byte at a time. Prints "compressed_offset decoded_offset" per
//! candidate and writes the decoded output. Candidates are validated separately
//! with the C decoder; this tool only proposes them.
use brotli_decompressor::{
    Allocator, BrotliDecompressStream, BrotliResult, BrotliState, SliceWrapperMut, StandardAlloc,
};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let enc = std::fs::read(&a[1]).unwrap();
    let dict = if a[2] == "-" {
        Vec::new()
    } else {
        std::fs::read(&a[2]).unwrap()
    };
    let mut alloc = StandardAlloc::default();
    let mut d = <StandardAlloc as Allocator<u8>>::alloc_cell(&mut alloc, dict.len());
    d.slice_mut().copy_from_slice(&dict);
    let mut s = BrotliState::new_with_custom_dictionary(
        StandardAlloc::default(),
        StandardAlloc::default(),
        StandardAlloc::default(),
        d,
    );
    // A fresh decoder's header substate is HEADER_NONE; the enum type is private.
    let fresh = BrotliState::new(
        StandardAlloc::default(),
        StandardAlloc::default(),
        StandardAlloc::default(),
    );
    let header_none = std::mem::discriminant(&fresh.substate_metablock_header);
    let mut out = vec![0u8; 1 << 16];
    let mut decoded = Vec::new();
    let mut total = 0usize;
    for k in 0..enc.len() {
        let mut avail_in = 1usize;
        let mut in_off = k;
        loop {
            let mut avail_out = out.len();
            let mut out_off = 0usize;
            let r = BrotliDecompressStream(
                &mut avail_in,
                &mut in_off,
                &enc,
                &mut avail_out,
                &mut out_off,
                &mut out,
                &mut total,
                &mut s,
            );
            decoded.extend_from_slice(&out[..out_off]);
            match r {
                BrotliResult::NeedsMoreOutput => continue,
                BrotliResult::ResultFailure => panic!("decode failure at {k}"),
                _ => break,
            }
        }
        let st = format!("{:?}", s.state);
        let between = st == "BROTLI_STATE_METABLOCK_BEGIN"
            || (st == "BROTLI_STATE_METABLOCK_HEADER"
                && std::mem::discriminant(&s.substate_metablock_header) == header_none);
        let bits = 64 - s.br.bit_pos_;
        if between && bits == 0 && s.br.avail_in == 0 {
            println!("{} {}", k + 1, decoded.len());
        }
    }
    std::fs::write(&a[3], &decoded).unwrap();
}
