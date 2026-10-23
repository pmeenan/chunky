//! `chunky inspect`: the meta-block structure of a `br` stream, decoded by
//! the independent inspector in `chunky_core::inspect`. Prints one table row
//! (or, with `--json`, one JSON line) per meta-block, then totals and the
//! SHA-256 of the decoded body.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chunky_core::inspect::{BlockKind, Inspection, MetaBlock, inspect};
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    /// Brotli stream (raw `br`; strip any dcb header first).
    stream: PathBuf,
    /// JSON lines: one object per meta-block, then a summary object.
    #[arg(long)]
    json: bool,
}

pub fn run(a: Args) -> Result<()> {
    let enc = std::fs::read(&a.stream).with_context(|| format!("read {}", a.stream.display()))?;
    let ins = inspect(&enc).with_context(|| format!("decode {}", a.stream.display()))?;
    let sha = crate::baseline::sha256_hex(&ins.decoded);
    let name = a.stream.display().to_string();
    let out = if a.json {
        json_lines(&ins, &name, enc.len(), &sha)
    } else {
        table(&ins, &name, enc.len(), &sha)
    };
    print!("{out}");
    Ok(())
}

/// Per-stream sums of the per-block counters.
#[derive(Default)]
struct Totals {
    header_bits: u64,
    pad_bits: u64,
    metadata_len: u64,
    commands: u64,
    literals: u64,
    copies: u64,
    copy_bytes: u64,
    dict_refs: u64,
    dict_bytes: u64,
    last_distance_codes: u64,
    block_switches: [u64; 3],
    literal_bits: u64,
    command_bits: u64,
    distance_bits: u64,
    block_switch_bits: u64,
}

impl Totals {
    fn of(blocks: &[MetaBlock]) -> Self {
        let mut t = Totals::default();
        for b in blocks {
            t.header_bits += b.header_bits;
            t.pad_bits += b.pad_bits;
            t.metadata_len += b.metadata_len;
            t.commands += b.commands;
            t.literals += b.literals;
            t.copies += b.copies;
            t.copy_bytes += b.copy_bytes;
            t.dict_refs += b.dict_refs;
            t.dict_bytes += b.dict_bytes;
            t.last_distance_codes += b.last_distance_codes;
            for i in 0..3 {
                t.block_switches[i] += b.block_switches[i];
            }
            t.literal_bits += b.literal_bits;
            t.command_bits += b.command_bits;
            t.distance_bits += b.distance_bits;
            t.block_switch_bits += b.block_switch_bits;
        }
        t
    }
}

/// Bits as bytes with one decimal.
fn by(bits: u64) -> String {
    format!("{:.1}", bits as f64 / 8.0)
}

fn table(ins: &Inspection, name: &str, stream_len: usize, sha: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{name}: {stream_len} B -> {} B (ratio {:.4}), wbits {}, {} meta-blocks",
        ins.decoded.len(),
        stream_len as f64 / ins.decoded.len().max(1) as f64,
        ins.wbits,
        ins.blocks.len()
    );
    let _ = writeln!(s, "decoded sha256 {sha}");
    let _ = writeln!(
        s,
        "{:>5} {:<12} {:>4} {:>10} {:>9} {:>8} {:>9} {:>8} {:>9} {:>5} {:>6} {:>7} {:>6} {:>5} {:>6} {:>9} {:>8} {:>8} {:>6}",
        "#",
        "kind",
        "last",
        "start_B",
        "bytes",
        "mlen",
        "out_off",
        "hdr_B",
        "nbl_LID",
        "tr_LD",
        "cmds",
        "lits",
        "copies",
        "dict",
        "short",
        "lit_B",
        "cmd_B",
        "dist_B",
        "sw_B"
    );
    for (i, b) in ins.blocks.iter().enumerate() {
        let (nbl, trees) = if b.kind == BlockKind::Compressed {
            (
                format!("{}/{}/{}", b.nbltypes[0], b.nbltypes[1], b.nbltypes[2]),
                format!("{}/{}", b.ntrees_lit, b.ntrees_dist),
            )
        } else {
            ("-".to_string(), "-".to_string())
        };
        let _ = writeln!(
            s,
            "{:>5} {:<12} {:>4} {:>10} {:>9} {:>8} {:>9} {:>8} {:>9} {:>5} {:>6} {:>7} {:>6} {:>5} {:>6} {:>9} {:>8} {:>8} {:>6}",
            i,
            b.kind.as_str(),
            if b.is_last { "y" } else { "" },
            by(b.start_bit),
            by(b.end_bit - b.start_bit),
            b.mlen,
            b.out_start,
            by(b.header_bits),
            nbl,
            trees,
            b.commands,
            b.literals,
            b.copies,
            b.dict_refs,
            b.last_distance_codes,
            by(b.literal_bits),
            by(b.command_bits),
            by(b.distance_bits),
            by(b.block_switch_bits)
        );
    }
    let t = Totals::of(&ins.blocks);
    let _ = writeln!(
        s,
        "{:>5} {:<12} {:>4} {:>10} {:>9} {:>8} {:>9} {:>8} {:>9} {:>5} {:>6} {:>7} {:>6} {:>5} {:>6} {:>9} {:>8} {:>8} {:>6}",
        "total",
        "",
        "",
        "",
        by(ins.total_bits),
        ins.decoded.len(),
        "",
        by(t.header_bits),
        "",
        "",
        t.commands,
        t.literals,
        t.copies,
        t.dict_refs,
        t.last_distance_codes,
        by(t.literal_bits),
        by(t.command_bits),
        by(t.distance_bits),
        by(t.block_switch_bits)
    );
    let _ = writeln!(
        s,
        "bytes: stream header {}, headers {}, padding {}, metadata {}, literals {}, commands {}, distances {}, switches {}",
        by(ins.stream_header_bits),
        by(t.header_bits),
        by(t.pad_bits),
        t.metadata_len,
        by(t.literal_bits),
        by(t.command_bits),
        by(t.distance_bits),
        by(t.block_switch_bits)
    );
    let _ = writeln!(
        s,
        "output: {} literal, {} copied, {} from {} dictionary refs; block switches L/I/D {}/{}/{}",
        t.literals,
        t.copy_bytes,
        t.dict_bytes,
        t.dict_refs,
        t.block_switches[0],
        t.block_switches[1],
        t.block_switches[2]
    );
    s
}

fn json_lines(ins: &Inspection, name: &str, stream_len: usize, sha: &str) -> String {
    let mut s = String::new();
    for (i, b) in ins.blocks.iter().enumerate() {
        let v = json!({
            "type": "meta_block", "index": i, "kind": b.kind.as_str(), "is_last": b.is_last,
            "start_bit": b.start_bit, "end_bit": b.end_bit,
            "out_start": b.out_start, "mlen": b.mlen,
            "header_bits": b.header_bits, "pad_bits": b.pad_bits,
            "metadata_len": b.metadata_len,
            "nbltypes": b.nbltypes, "npostfix": b.npostfix, "ndirect": b.ndirect,
            "context_modes": b.context_modes,
            "ntrees_lit": b.ntrees_lit, "ntrees_dist": b.ntrees_dist,
            "commands": b.commands, "literals": b.literals,
            "copies": b.copies, "copy_bytes": b.copy_bytes,
            "dict_refs": b.dict_refs, "dict_bytes": b.dict_bytes,
            "last_distance_codes": b.last_distance_codes,
            "block_switches": b.block_switches,
            "literal_bits": b.literal_bits, "command_bits": b.command_bits,
            "distance_bits": b.distance_bits, "block_switch_bits": b.block_switch_bits,
        });
        let _ = writeln!(s, "{v}");
    }
    let t = Totals::of(&ins.blocks);
    let v = json!({
        "type": "summary", "file": name, "stream_bytes": stream_len,
        "decoded_bytes": ins.decoded.len(), "decoded_sha256": sha,
        "wbits": ins.wbits, "stream_header_bits": ins.stream_header_bits,
        "total_bits": ins.total_bits, "meta_blocks": ins.blocks.len(),
        "header_bits": t.header_bits, "pad_bits": t.pad_bits,
        "metadata_len": t.metadata_len,
        "commands": t.commands, "literals": t.literals,
        "copies": t.copies, "copy_bytes": t.copy_bytes,
        "dict_refs": t.dict_refs, "dict_bytes": t.dict_bytes,
        "last_distance_codes": t.last_distance_codes,
        "block_switches": t.block_switches,
        "literal_bits": t.literal_bits, "command_bits": t.command_bits,
        "distance_bits": t.distance_bits, "block_switch_bits": t.block_switch_bits,
    });
    let _ = writeln!(s, "{v}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_json_cover_every_block() {
        let input: Vec<u8> = (0..3000u32)
            .flat_map(|i| format!("<li id=\"{}\">item {}</li>", i % 97, i % 13).into_bytes())
            .collect();
        let p = brotli_c::EncoderParams::new(5, 18);
        let enc = brotli_c::compress_with_flushes(&p, None, &input, &[1000, 20000]).unwrap();
        let ins = inspect(&enc).unwrap();
        assert_eq!(ins.decoded, input);
        let sha = crate::baseline::sha256_hex(&ins.decoded);

        let t = table(&ins, "x.br", enc.len(), &sha);
        assert!(t.contains(&sha));
        // Header, sha, column names, one row per block, totals, two summaries.
        assert_eq!(t.lines().count(), 3 + ins.blocks.len() + 3);

        let j = json_lines(&ins, "x.br", enc.len(), &sha);
        let rows: Vec<serde_json::Value> = j
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), ins.blocks.len() + 1);
        let summary = rows.last().unwrap();
        assert_eq!(summary["decoded_sha256"], sha.as_str());
        assert_eq!(summary["decoded_bytes"], input.len());
        let mlen: u64 = rows[..rows.len() - 1]
            .iter()
            .map(|r| r["mlen"].as_u64().unwrap())
            .sum();
        assert_eq!(mlen, input.len() as u64);
    }
}
