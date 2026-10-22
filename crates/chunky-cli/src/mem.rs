//! `chunky mem`: D-025 memory baselines. Per-stream incremental heap memory
//! of C brotli (counted through its allocator hooks) and rust-brotli
//! (counted by the CLI's global allocator), by quality, window and response
//! size, plus the shared cost of one C prepared dictionary. Allocation
//! counts are deterministic, so this needs only a shared hold; timings are
//! `chunky bench`'s job.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use chunky_core::reference::{ReferenceEncoder, ReferenceParams};

use crate::alloc_count;
use crate::corpus::{self, Dicts};
use crate::encoders::{Schedule, drive};
use crate::summarize::fmt_int;

#[derive(clap::Args)]
pub struct Args {
    /// Manifest directories; the median and largest responses are used.
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    #[arg(long, default_value = "1,2,3,4,5,6,9,11")]
    qualities: String,
    #[arg(long, value_delimiter = ',', default_value = "18,22")]
    lgwins: Vec<u32>,
}

pub fn run(a: Args) -> Result<()> {
    let mut rows = corpus::load(&a.dirs)?;
    rows.sort_by_key(|r| r.decoded_bytes);
    let median = rows
        .get(rows.len() / 2)
        .ok_or_else(|| anyhow!("empty corpus"))?
        .clone();
    let largest = rows.last().unwrap().clone();
    let tiny: Vec<u8> = median.body()?[..1024.min(median.decoded_bytes)].to_vec();
    let inputs = [
        ("1 KiB", tiny, vec![]),
        ("median", median.body()?, median.flushes.clone()),
        ("largest", largest.body()?, largest.flushes.clone()),
    ];
    println!(
        "Per-stream heap peak, served flush schedule (median {} B, largest {} B).\n",
        fmt_int(median.decoded_bytes as u64),
        fmt_int(largest.decoded_bytes as u64)
    );
    println!("| Engine | Quality | lgwin | 1 KiB | median | largest |");
    println!("| --- | --- | --- | --- | --- | --- |");
    let qs = crate::baseline::parse_qualities(&a.qualities)?;
    for engine in ["c", "rust"] {
        for &q in &qs {
            for &w in &a.lgwins {
                let mut cells = Vec::new();
                for (_, input, flushes) in &inputs {
                    let sched = Schedule {
                        name: "served",
                        cuts: flushes.clone(),
                        flush: true,
                    };
                    cells.push(if engine == "c" {
                        c_peak(q, w, input, &sched, None)?
                    } else {
                        rust_peak(q, w, input, &sched)?
                    });
                }
                println!(
                    "| {engine} | q{q} | {w} | {} | {} | {} |",
                    mib(cells[0]),
                    mib(cells[1]),
                    mib(cells[2])
                );
            }
        }
    }
    // Shared prepared dictionary (C) and per-stream peak with it attached.
    let dicts = Dicts::load(&rows)?;
    if let Some(r) = rows.iter().rev().find(|r| dicts.get(r).is_some()) {
        let d = dicts.get(r).unwrap();
        println!(
            "\nC prepared dictionary ({} B raw, shared by all streams):\n",
            fmt_int(d.len() as u64)
        );
        println!(
            "| Prepared for quality | Index heap | Stream peak with it attached (q, lgwin 22, largest dcb page) |"
        );
        println!("| --- | --- | --- |");
        let input = r.body()?;
        let sched = Schedule {
            name: "served",
            cuts: r.flushes.clone(),
            flush: true,
        };
        for q in [5u32, 9, 11] {
            let pd = brotli_c::PreparedDictionary::new(d, q)?;
            let peak = c_peak(q, 22, &input, &sched, Some(&pd))?;
            println!("| q{q} | {} | {} |", mib(pd.mem().peak()), mib(peak));
        }
    }
    Ok(())
}

fn c_peak(
    q: u32,
    w: u32,
    input: &[u8],
    sched: &Schedule,
    pd: Option<&brotli_c::PreparedDictionary<'_>>,
) -> Result<usize> {
    let mut e = brotli_c::Encoder::new(&brotli_c::EncoderParams::new(q, w))?;
    if let Some(d) = pd {
        e.attach(d)?;
    }
    let mut out = Vec::with_capacity(input.len() + 4096);
    drive(input, sched, |c, op| {
        let op = match op {
            chunky_core::reference::Op::Process => brotli_c::Op::Process,
            chunky_core::reference::Op::Flush => brotli_c::Op::Flush,
            chunky_core::reference::Op::Finish => brotli_c::Op::Finish,
        };
        e.push(c, op, &mut out).map_err(|e| anyhow!("{e}"))
    })?;
    Ok(e.mem().peak())
}

fn rust_peak(q: u32, w: u32, input: &[u8], sched: &Schedule) -> Result<usize> {
    let mut out = Vec::with_capacity(input.len() + (1 << 20));
    let base = alloc_count::reset_peak();
    let mut e = ReferenceEncoder::new(&ReferenceParams::new(q, w))?;
    drive(input, sched, |c, op| {
        e.push(c, op, &mut out).map_err(|e| anyhow!("{e}"))
    })?;
    let peak = alloc_count::peak() - base;
    drop(e);
    Ok(peak)
}

fn mib(n: usize) -> String {
    format!("{:.2} MiB", n as f64 / (1 << 20) as f64)
}
