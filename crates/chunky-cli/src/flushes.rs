//! `chunky flushes`: propose byte-aligned meta-block boundaries in a served
//! brotli stream by feeding the rust-brotli decoder one byte at a time
//! (formerly tools/netlog/flushscan). Prints "stream_offset decoded_offset"
//! per candidate and writes the decoded output. The netlog extractor
//! validates every candidate with the C decoder; this only proposes them.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use brotli_decompressor::{
    Allocator, BrotliDecompressStream, BrotliResult, BrotliState, SliceWrapperMut, StandardAlloc,
};

#[derive(clap::Args)]
pub struct Args {
    /// Served brotli stream (dcb header already stripped).
    stream: PathBuf,
    /// Raw shared dictionary, or "-" for none.
    dictionary: String,
    /// Where to write the decoded bytes.
    decoded_out: PathBuf,
}

pub fn run(a: Args) -> Result<()> {
    let enc = std::fs::read(&a.stream).with_context(|| format!("read {}", a.stream.display()))?;
    let dict = if a.dictionary == "-" {
        Vec::new()
    } else {
        std::fs::read(&a.dictionary).with_context(|| format!("read {}", a.dictionary))?
    };
    let (cands, decoded) = scan(&enc, &dict)?;
    let mut out = String::new();
    for (s, d) in cands {
        out.push_str(&format!("{s} {d}\n"));
    }
    print!("{out}");
    std::fs::write(&a.decoded_out, decoded)?;
    Ok(())
}

/// (stream_offset, decoded_offset) boundary candidates.
pub type Candidates = Vec<(usize, usize)>;

/// Returns the boundary candidates and the decoded bytes.
pub fn scan(enc: &[u8], dict: &[u8]) -> Result<(Candidates, Vec<u8>)> {
    let mut alloc = StandardAlloc::default();
    let mut d = <StandardAlloc as Allocator<u8>>::alloc_cell(&mut alloc, dict.len());
    d.slice_mut().copy_from_slice(dict);
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
    let mut cands = Vec::new();
    for k in 0..enc.len() {
        let mut avail_in = 1usize;
        let mut in_off = k;
        loop {
            let mut avail_out = out.len();
            let mut out_off = 0usize;
            let r = BrotliDecompressStream(
                &mut avail_in,
                &mut in_off,
                enc,
                &mut avail_out,
                &mut out_off,
                &mut out,
                &mut total,
                &mut s,
            );
            decoded.extend_from_slice(&out[..out_off]);
            match r {
                BrotliResult::NeedsMoreOutput => continue,
                BrotliResult::ResultFailure => bail!("decode failure at stream offset {k}"),
                _ => break,
            }
        }
        let st = format!("{:?}", s.state);
        let between = st == "BROTLI_STATE_METABLOCK_BEGIN"
            || (st == "BROTLI_STATE_METABLOCK_HEADER"
                && std::mem::discriminant(&s.substate_metablock_header) == header_none);
        let bits = 64 - s.br.bit_pos_;
        if between && bits == 0 && s.br.avail_in == 0 {
            cands.push((k + 1, decoded.len()));
        }
    }
    Ok((cands, decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chunky_core::reference::{ReferenceParams, compress_with_flushes};

    #[test]
    fn finds_every_flush_of_a_reference_stream() {
        let input: Vec<u8> = (0..30_000u32)
            .map(|i| (i % 251) as u8 ^ (i / 997) as u8)
            .collect();
        let flushes = [1000, 5000, 12_345, 29_000];
        let enc = compress_with_flushes(&ReferenceParams::new(5, 22), &input, &flushes).unwrap();
        let (cands, decoded) = scan(&enc, &[]).unwrap();
        assert_eq!(decoded, input);
        let found: Vec<usize> = cands.iter().map(|c| c.1).collect();
        for f in flushes {
            assert!(found.contains(&f), "flush at {f} not found in {found:?}");
        }
        for (s, d) in cands {
            let mut prefix = enc[..s].to_vec();
            prefix.push(3);
            if d < input.len() {
                assert_eq!(brotli_c::decode(&prefix, None).unwrap(), &input[..d]);
            }
        }
    }
}
