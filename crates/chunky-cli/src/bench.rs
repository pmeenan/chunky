//! `chunky bench`: CPU side of the reference curve (D-016, D-023).
//!
//! Re-executes itself under `hostlock exclusive` pinned to one core with
//! `taskset`, so no other heavy work on the host skews it. Each config runs
//! one warm-up pass over the sample, then `--reps` timed passes (at least
//! 10), reporting the median and median absolute deviation. Also times
//! per-response setup (D-025): encoder create/destroy, a whole 1 KiB
//! response, and C dictionary preparation.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::corpus::{self, Dicts, Row};
use crate::encoders::{Config, Ctx, Engine, schedule};
use crate::summarize::fmt_int;

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    #[arg(long, value_delimiter = ',', default_value = "c,rust")]
    engines: Vec<String>,
    #[arg(long, default_value = "1,2,3,4,5,6,9,11")]
    qualities: String,
    #[arg(long, value_delimiter = ',', default_value = "22")]
    lgwins: Vec<u32>,
    #[arg(long, value_delimiter = ',', default_value = "buffered,served,reads")]
    schedules: Vec<String>,
    /// Also time C with the served dictionary (dcb rows only).
    #[arg(long)]
    dict: bool,
    /// Timed passes per config (D-023: at least 10).
    #[arg(long, default_value_t = 10)]
    reps: usize,
    /// Sample about this many decoded bytes, spread evenly over the corpus.
    #[arg(long, default_value_t = 8 << 20)]
    sample_bytes: usize,
    /// CPU to pin to.
    #[arg(long, default_value_t = 2)]
    cpu: usize,
    /// JSON output.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Smoke test only: skip the exclusive hold and pinning. Numbers are
    /// labeled unlocked and are not measurements.
    #[arg(long)]
    no_lock: bool,
}

const INNER: &str = "CHUNKY_BENCH_INNER";

pub fn run(a: Args) -> Result<()> {
    if a.reps == 0 || a.sample_bytes == 0 {
        bail!("--reps and --sample-bytes must be greater than zero");
    }
    if a.reps < 10 && !a.no_lock {
        bail!("--reps must be at least 10 (D-023)");
    }
    if !a.no_lock && std::env::var_os(INNER).is_none() {
        let exe = std::env::current_exe()?;
        let status = Command::new("hostlock")
            .args([
                "exclusive",
                "--label",
                "chunky: bench",
                "--",
                "taskset",
                "-c",
            ])
            .arg(a.cpu.to_string())
            .arg(exe)
            .args(std::env::args_os().skip(1))
            .env(INNER, "1")
            .status()
            .context("run hostlock (is it installed?)")?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let locked = !a.no_lock;
    let mut rows = corpus::load(&a.dirs)?;
    rows.sort_by(|x, y| x.body_sha256.cmp(&y.body_sha256));
    rows.dedup_by(|x, y| x.body_sha256 == y.body_sha256);
    let sample = pick(&rows, a.sample_bytes);
    if sample.is_empty() {
        bail!("no eligible responses in the benchmark corpus");
    }
    let bodies: Vec<Vec<u8>> = sample.iter().map(|r| r.body()).collect::<Result<_>>()?;
    let dicts = Dicts::load(&sample)?;
    let total: usize = bodies.iter().map(Vec::len).sum();
    let prov = crate::provenance::collect();
    println!("{}", crate::provenance::markdown(&prov));
    println!(
        "Sample: {} responses, {} decoded bytes; {} timed passes after 1 warm-up; {}.\n",
        sample.len(),
        fmt_int(total as u64),
        a.reps,
        if locked {
            format!("hostlock exclusive, pinned to CPU {}", a.cpu)
        } else {
            "UNLOCKED SMOKE TEST, not a measurement".into()
        }
    );
    let qs = crate::baseline::parse_qualities(&a.qualities)?;
    let mut results = Vec::new();
    println!("| Config | Schedule | Median ms | MAD ms | ms/MB | MB/s |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for e in &a.engines {
        let engine = Engine::parse(e)?;
        for &q in &qs {
            for &w in &a.lgwins {
                for dict in [false, true] {
                    if dict && (!a.dict || engine != Engine::C) {
                        continue;
                    }
                    let cfg = Config {
                        engine,
                        quality: q,
                        lgwin: w,
                        no_lcm: false,
                        dict,
                    };
                    for s in &a.schedules {
                        let (idx, bytes): (Vec<usize>, usize) = {
                            let idx: Vec<usize> = (0..sample.len())
                                .filter(|&i| !dict || sample[i].dictionary_sfv.is_some())
                                .collect();
                            let b = idx.iter().map(|&i| bodies[i].len()).sum();
                            (idx, b)
                        };
                        let scheds: Vec<_> = idx
                            .iter()
                            .map(|&i| schedule(s, &sample[i]))
                            .collect::<Result<_>>()?;
                        let mut ctx = Ctx::new();
                        let mut pass = || -> Result<f64> {
                            let t = Instant::now();
                            for (k, &i) in idx.iter().enumerate() {
                                let d = if dict {
                                    Some((
                                        sample[i].dictionary_path().unwrap(),
                                        dicts.get(&sample[i]).ok_or_else(|| anyhow!("no dict"))?,
                                    ))
                                } else {
                                    None
                                };
                                std::hint::black_box(ctx.encode(&cfg, &bodies[i], &scheds[k], d)?);
                            }
                            Ok(t.elapsed().as_secs_f64() * 1e3)
                        };
                        pass()?;
                        let times: Vec<f64> = (0..a.reps).map(|_| pass()).collect::<Result<_>>()?;
                        let (med, mad) = med_mad(&times);
                        let ms_per_mb = med / (bytes as f64 / 1e6);
                        println!(
                            "| {} | {s} | {med:.1} | {mad:.2} | {ms_per_mb:.2} | {:.1} |",
                            cfg.label(),
                            1e3 / ms_per_mb
                        );
                        results.push(
                            json!({"config": cfg.label(), "engine": engine.name(), "q": q,
                            "lgwin": w, "dict": dict, "schedule": s, "bytes": bytes,
                            "responses": idx.len(), "median_ms": med, "mad_ms": mad,
                            "ms_per_mb": ms_per_mb, "times_ms": times}),
                        );
                    }
                }
            }
        }
    }
    let setup = setup_times(&a, &qs, &sample, &bodies, &dicts)?;
    if let Some(p) = &a.out {
        std::fs::write(
            p,
            serde_json::to_string_pretty(&json!({"provenance": prov, "locked": locked,
                "cpu": a.cpu, "reps": a.reps, "sample_responses": sample.len(),
                "sample_bytes": total, "results": results, "setup": setup}))?,
        )?;
        eprintln!("wrote {}", p.display());
    }
    Ok(())
}

/// Rows spread evenly over the (hash-sorted) corpus until about `budget` bytes.
fn pick(rows: &[Row], budget: usize) -> Vec<Row> {
    let sizes: Vec<usize> = rows.iter().map(|r| r.decoded_bytes).collect();
    crate::corpus::spread_indices(&sizes, budget)
        .into_iter()
        .map(|i| rows[i].clone())
        .collect()
}

fn med_mad(v: &[f64]) -> (f64, f64) {
    let med = median(v.to_vec());
    let dev: Vec<f64> = v.iter().map(|x| (x - med).abs()).collect();
    (med, median(dev))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Per-response setup costs (D-025), medians over many iterations.
fn setup_times(
    a: &Args,
    qs: &[u32],
    sample: &[Row],
    bodies: &[Vec<u8>],
    dicts: &Dicts,
) -> Result<serde_json::Value> {
    let tiny: Vec<u8> = bodies
        .iter()
        .find(|b| b.len() >= 1024)
        .map(|b| b[..1024].to_vec())
        .unwrap_or_default();
    let one = crate::encoders::Schedule {
        name: "buffered",
        cuts: vec![],
        flush: false,
    };
    println!("\nPer-response setup (median of 200; µs):\n");
    println!("| Config | create + destroy | whole 1 KiB response |");
    println!("| --- | --- | --- |");
    let mut out = Vec::new();
    for e in &a.engines {
        let engine = Engine::parse(e)?;
        for &q in qs {
            for &w in &a.lgwins {
                let cfg = Config {
                    engine,
                    quality: q,
                    lgwin: w,
                    no_lcm: false,
                    dict: false,
                };
                let mut ctx = Ctx::new();
                let mut create = Vec::new();
                let mut whole = Vec::new();
                for i in 0..220 {
                    let t = Instant::now();
                    match engine {
                        Engine::C => drop(std::hint::black_box(brotli_c::Encoder::new(
                            &brotli_c::EncoderParams::new(q, w),
                        )?)),
                        Engine::Rust => drop(std::hint::black_box(
                            chunky_core::reference::ReferenceEncoder::new(
                                &chunky_core::reference::ReferenceParams::new(q, w),
                            )?,
                        )),
                    }
                    let c = t.elapsed().as_secs_f64() * 1e6;
                    let t = Instant::now();
                    std::hint::black_box(ctx.encode(&cfg, &tiny, &one, None)?);
                    let wt = t.elapsed().as_secs_f64() * 1e6;
                    if i >= 20 {
                        create.push(c);
                        whole.push(wt);
                    }
                }
                let (c, w1) = (median(create), median(whole));
                println!("| {} | {c:.1} | {w1:.1} |", cfg.label());
                out.push(json!({"config": cfg.label(), "create_destroy_us": c, "tiny_1k_us": w1}));
            }
        }
    }
    let mut prep = Vec::new();
    if let Some(r) = sample.iter().find(|r| dicts.get(r).is_some()) {
        let d = dicts.get(r).unwrap();
        println!(
            "\nC dictionary preparation ({} B), median of 10:\n",
            fmt_int(d.len() as u64)
        );
        println!("| Prepared for | Prepare ms | Attach µs |");
        println!("| --- | --- | --- |");
        for q in [5u32, 11] {
            let mut pt = Vec::new();
            let mut at = Vec::new();
            for _ in 0..10 {
                let t = Instant::now();
                let pd = brotli_c::PreparedDictionary::new(d, q)?;
                pt.push(t.elapsed().as_secs_f64() * 1e3);
                let mut e = brotli_c::Encoder::new(&brotli_c::EncoderParams::new(q, 22))?;
                let t = Instant::now();
                e.attach(&pd)?;
                at.push(t.elapsed().as_secs_f64() * 1e6);
            }
            let (p, t) = (median(pt), median(at));
            println!("| q{q} | {p:.2} | {t:.1} |");
            prep.push(
                json!({"quality": q, "prepare_ms": p, "attach_us": t, "dict_bytes": d.len()}),
            );
        }
    }
    Ok(json!({"per_config": out, "dictionary": prep}))
}
