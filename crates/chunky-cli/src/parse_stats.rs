//! `chunky parse-stats`: what a rust-brotli parse is made of, from the
//! encoder's meta-block callback: literal, copy and static-dictionary byte
//! shares, and copy bytes by distance (the decomposition's static-dictionary
//! share and the corpus side of effective reach). C exposes no parse, so this
//! is rust-brotli only and labeled as such.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use brotli::enc::StandardAlloc;
use brotli::enc::encode::{
    BrotliEncoderOperation, BrotliEncoderParameter, BrotliEncoderStateStruct,
};
use brotli::enc::interface::{Command, PredictionModeContextMap, StaticCommand};
use brotli::{InputPair, InputReferenceMut};

use crate::corpus;
use crate::encoders::{Schedule, drive, schedule};
use crate::summarize::fmt_int;
use chunky_core::reference::Op;

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    #[arg(long, default_value = "2,5,9,11")]
    qualities: String,
    #[arg(long, default_value_t = 22)]
    lgwin: u32,
    #[arg(long, default_value = "buffered")]
    schedule: String,
}

const BUCKETS: [(usize, &str); 6] = [
    (1 << 12, "≤4K"),
    (1 << 16, "≤64K"),
    (1 << 18, "≤256K"),
    (1 << 20, "≤1M"),
    (1 << 22, "≤4M"),
    (usize::MAX, ">4M"),
];

#[derive(Default, Clone)]
struct Tally {
    literal: u64,
    copy: u64,
    dict: u64,
    dict_refs: u64,
    by_dist: [u64; 6],
}

pub fn run(a: Args) -> Result<()> {
    let rows = corpus::load(&a.dirs)?;
    let qs = crate::baseline::parse_qualities(&a.qualities)?;
    let sites: std::collections::BTreeSet<String> = rows.iter().map(|r| r.site.clone()).collect();
    println!(
        "rust-brotli parse composition, lgwin {}, {} schedule; % of decoded bytes.\n",
        a.lgwin, a.schedule
    );
    print!("| Site | Quality | Literals | Copies | Static dict | Dict refs |");
    for (_, n) in BUCKETS {
        print!(" copy {n} |");
    }
    println!();
    println!(
        "| --- | --- | --- | --- | --- | --- |{}",
        " --- |".repeat(BUCKETS.len())
    );
    for site in &sites {
        for &q in &qs {
            let mut t = Tally::default();
            for r in rows.iter().filter(|r| &r.site == site) {
                let body = r.body()?;
                let s = schedule(&a.schedule, r)?;
                tally(q, a.lgwin, &body, &s, &mut t)?;
            }
            let total = (t.literal + t.copy + t.dict) as f64;
            let pct = |x: u64| format!("{:.1}%", 100.0 * x as f64 / total);
            print!(
                "| {site} | q{q} | {} | {} | {} | {} |",
                pct(t.literal),
                pct(t.copy),
                pct(t.dict),
                fmt_int(t.dict_refs)
            );
            for b in t.by_dist {
                print!(" {} |", pct(b));
            }
            println!();
        }
    }
    Ok(())
}

fn tally(q: u32, w: u32, input: &[u8], sched: &Schedule, t: &mut Tally) -> Result<()> {
    use BrotliEncoderParameter::*;
    let mut s = BrotliEncoderStateStruct::new(StandardAlloc::default());
    if !(s.set_parameter(BROTLI_PARAM_QUALITY, q)
        && s.set_parameter(BROTLI_PARAM_LGWIN, w)
        && s.set_parameter(BROTLI_METABLOCK_CALLBACK, 1))
    {
        return Err(anyhow!("parameter rejected"));
    }
    let mut cb = |_: &mut PredictionModeContextMap<InputReferenceMut>,
                  cmds: &mut [StaticCommand],
                  _: InputPair,
                  _: &mut StandardAlloc| {
        for c in cmds.iter() {
            match c {
                Command::Literal(l) => t.literal += l.data.1 as u64,
                Command::Copy(c) => {
                    t.copy += c.num_bytes as u64;
                    let i = BUCKETS
                        .iter()
                        .position(|(lim, _)| (c.distance as usize) <= *lim)
                        .unwrap();
                    t.by_dist[i] += c.num_bytes as u64;
                }
                Command::Dict(d) => {
                    t.dict += d.final_size as u64;
                    t.dict_refs += 1;
                }
                _ => {}
            }
        }
    };
    let mut scratch = vec![0u8; 1 << 16];
    // Same call sequence as every other measurement (`encoders::drive`).
    drive(input, sched, |chunk, op| {
        let op = match op {
            Op::Process => BrotliEncoderOperation::BROTLI_OPERATION_PROCESS,
            Op::Flush => BrotliEncoderOperation::BROTLI_OPERATION_FLUSH,
            Op::Finish => BrotliEncoderOperation::BROTLI_OPERATION_FINISH,
        };
        let mut avail_in = chunk.len();
        let mut in_off = 0;
        loop {
            let mut avail_out = scratch.len();
            let mut out_off = 0;
            let mut total = None;
            if !s.compress_stream(
                op,
                &mut avail_in,
                chunk,
                &mut in_off,
                &mut avail_out,
                &mut scratch,
                &mut out_off,
                &mut total,
                &mut cb,
            ) {
                return Err(anyhow!("compress_stream failed"));
            }
            if avail_in == 0 && !s.has_more_output() {
                if op == BrotliEncoderOperation::BROTLI_OPERATION_FINISH && !s.is_finished() {
                    continue;
                }
                return Ok(());
            }
        }
    })?;
    Ok(())
}
