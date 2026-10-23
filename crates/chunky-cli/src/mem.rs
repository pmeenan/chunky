//! `chunky mem`: D-025 memory baselines. Per-stream incremental heap memory
//! of C brotli (counted through its allocator hooks) and rust-brotli
//! (counted by the CLI's global allocator), by quality, window and response
//! size, plus the shared cost of one C prepared dictionary, and chunky's
//! stream encoder along each of its knobs (D-029). Allocation
//! counts are deterministic, so this needs only a shared hold; timings are
//! `chunky bench`'s job.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use chunky_core::reference::{ReferenceEncoder, ReferenceParams, capture_parse};
use chunky_core::stream::{Engine, ExternalCommand, StreamOptions, Workspace, flatten_parse};

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
    /// Skip the reference encoders and sweep only chunky's stream knobs.
    #[arg(long)]
    stream_only: bool,
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
    let qs = crate::baseline::parse_qualities(&a.qualities)?;
    let engines: &[&str] = if a.stream_only { &[] } else { &["c", "rust"] };
    if !engines.is_empty() {
        println!("| Engine | Quality | lgwin | 1 KiB | median | largest |");
        println!("| --- | --- | --- | --- | --- | --- |");
    }
    for &engine in engines {
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
    stream_sweep(&inputs)?;
    if a.stream_only {
        return Ok(());
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

/// chunky's stream encoder, one knob at a time around the cold default
/// (lgwin 22, 22-bit retained history, compact ring, 64 KiB blocks, q2
/// search and tables). "injected" rows feed the response's buffered q11
/// parse instead of searching (no hasher); capturing it is not counted.
fn stream_sweep(inputs: &[(&str, Vec<u8>, Vec<usize>)]) -> Result<()> {
    let base = StreamOptions::cold(22);
    let mut rows: Vec<(String, StreamOptions, bool)> = Vec::new();
    for r in [16, 18, 20, 22] {
        for compact_ring in [true, false] {
            if compact_ring && r <= base.block_bits {
                continue; // a compact ring must exceed one block
            }
            let label = format!(
                "history {r} bits, {} ring",
                if compact_ring { "compact" } else { "full" }
            );
            rows.push((
                label,
                StreamOptions {
                    retained_bits: r,
                    compact_ring,
                    ..base
                },
                false,
            ));
        }
    }
    for b in [14, 18, 20] {
        rows.push((
            format!("blocks {b} bits"),
            StreamOptions {
                block_bits: b,
                ..base
            },
            false,
        ));
    }
    for q in [3, 4, 5, 6, 9] {
        rows.push((
            format!("search and tables q{q}"),
            StreamOptions {
                search_quality: q,
                table_quality: q,
                ..base
            },
            false,
        ));
    }
    for tq in [2, 5, 11] {
        rows.push((
            format!("search q2, tables q{tq}"),
            StreamOptions {
                table_quality: tq,
                ..base
            },
            false,
        ));
        rows.push((
            format!("injected q11 parse, tables q{tq}"),
            StreamOptions {
                table_quality: tq,
                search_quality: 0,
                ..base
            },
            true,
        ));
    }
    let parses: Vec<Vec<ExternalCommand>> = inputs
        .iter()
        .map(|(_, input, _)| {
            capture_parse(&ReferenceParams::new(11, 22), input, &[])
                .map(|(_, blocks)| flatten_parse(&blocks))
                .map_err(|e| anyhow!("{e}"))
        })
        .collect::<Result<_>>()?;
    println!(
        "\nchunky stream memory on the served flush schedule, by class (D-029): \
         per stream (held for the response's life) and per thread (the reused \
         workspace, retained at its high-water size, plus the transient peak \
         while a block is written). M2 has no shared engine state yet.\n"
    );
    println!(
        "| Config | Per stream, 1 KiB | Per stream, median | Per stream, largest | \
         Per thread retained, largest | Per thread transient, largest |"
    );
    println!("| --- | --- | --- | --- | --- | --- |");
    for (label, opts, injected) in rows {
        let mut cells = Vec::new();
        for ((_, input, flushes), parse) in inputs.iter().zip(&parses) {
            let sched = Schedule {
                name: "served",
                cuts: flushes.clone(),
                flush: true,
            };
            cells.push(stream_peak(
                opts,
                input,
                &sched,
                injected.then(|| parse.clone()),
            )?);
        }
        println!(
            "| {label} | {} | {} | {} | {} | {} |",
            mib(cells[0].stream),
            mib(cells[1].stream),
            mib(cells[2].stream),
            mib(cells[2].workspace),
            mib(cells[2].transient)
        );
    }
    Ok(())
}

/// One stream's memory by class.
struct Split {
    /// Held by the stream until it is dropped (ring, hasher, carry state).
    stream: usize,
    /// Held by the workspace after the response (block scratch high-water).
    workspace: usize,
    /// Peak above both while encoding (table construction temporaries).
    transient: usize,
}

fn stream_peak(
    opts: StreamOptions,
    input: &[u8],
    sched: &Schedule,
    parse: Option<Vec<ExternalCommand>>,
) -> Result<Split> {
    let engine = Engine::new(opts).map_err(|e| anyhow!("{e}"))?;
    let mut out = Vec::with_capacity(input.len() + (1 << 20));
    // The parse is allocated before the reset, so it is not counted, but
    // dropping the stream frees it: add it back when reading the heap after.
    let parse_bytes = parse
        .as_ref()
        .map_or(0, |p| p.capacity() * std::mem::size_of::<ExternalCommand>());
    let base = alloc_count::reset_peak();
    let mut ws = Workspace::default();
    let mut s = match parse {
        Some(p) => engine.stream_with_parse(p),
        None => engine.stream(),
    };
    drive(input, sched, |c, op| {
        s.feed(&mut ws, c, &mut out)?;
        match op {
            chunky_core::reference::Op::Process => Ok(()),
            chunky_core::reference::Op::Flush => s.flush(&mut ws, &mut out),
            chunky_core::reference::Op::Finish => s.finish(&mut ws, &mut out),
        }
        .map_err(|e| anyhow!("{e}"))
    })?;
    let peak = alloc_count::peak() - base;
    // reset_peak returns the current heap size.
    let both = alloc_count::reset_peak() - base;
    drop(s);
    let ws_only = (alloc_count::reset_peak() + parse_bytes).saturating_sub(base);
    drop(ws);
    Ok(Split {
        stream: both.saturating_sub(ws_only),
        workspace: ws_only,
        transient: peak.saturating_sub(both),
    })
}

fn mib(n: usize) -> String {
    format!("{:.2} MiB", n as f64 / (1 << 20) as f64)
}
