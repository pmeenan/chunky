//! `chunky baseline`: the size side of the reference quality curve (D-016).
//!
//! Encodes every corpus response with each configuration along each call
//! schedule, decodes every output with the C reference decoder (with the
//! dictionary attached for dictionary configs), and writes one JSON row per
//! (response, config, schedule). Size results are deterministic, so jobs run
//! in parallel; CPU numbers come from `chunky bench`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::corpus::{self, Dicts, Row};
use crate::encoders::{Config, Ctx, Engine, schedule};

#[derive(clap::Args)]
pub struct Args {
    /// Manifest directories (sessions or slices).
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    /// JSONL output, one row per (response, config, schedule).
    #[arg(long)]
    out: PathBuf,
    /// Engines: c, rust.
    #[arg(long, value_delimiter = ',', default_value = "c,rust")]
    engines: Vec<String>,
    /// Qualities, e.g. 0-11 or 1,2,5,11.
    #[arg(long, default_value = "0-11")]
    qualities: String,
    #[arg(long, value_delimiter = ',', default_value = "22")]
    lgwins: Vec<u32>,
    /// Call schedules: buffered, served, reads, reads-noflush, every4k.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "buffered,served,reads,reads-noflush"
    )]
    schedules: Vec<String>,
    /// Also run each config with literal context modeling disabled.
    #[arg(long)]
    no_lcm: bool,
    /// Also run C configs with the served dictionary attached (dcb rows only).
    #[arg(long)]
    dict: bool,
    /// Only these sites (default all).
    #[arg(long, value_delimiter = ',')]
    sites: Vec<String>,
    /// Only rows in these held-out-origin splits (multi-origin manifests).
    #[arg(long, value_delimiter = ',')]
    splits: Vec<String>,
    /// Deterministic sample: at most this many distinct bodies per site and
    /// size bucket, spread over the body hashes (0 = all).
    #[arg(long, default_value_t = 0)]
    max_per_bucket: usize,
    #[arg(long, default_value_t = default_threads())]
    threads: usize,
}

fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(2).max(1))
}

pub fn parse_qualities(s: &str) -> Result<Vec<u32>> {
    let mut v = Vec::new();
    for part in s.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            v.extend(a.parse::<u32>()?..=b.parse::<u32>()?);
        } else {
            v.push(part.parse()?);
        }
    }
    if v.iter().any(|&q| q > 11) {
        bail!("quality above 11");
    }
    Ok(v)
}

pub fn run(a: Args) -> Result<()> {
    let mut rows = corpus::load(&a.dirs)?;
    if !a.sites.is_empty() {
        rows.retain(|r| a.sites.contains(&r.site));
    }
    if !a.splits.is_empty() {
        rows.retain(|r| a.splits.contains(&r.split));
    }
    if a.max_per_bucket > 0 {
        rows = sample_per_bucket(rows, a.max_per_bucket);
    }
    // Largest first so the slow tail does not end up on one thread.
    rows.sort_by_key(|r| std::cmp::Reverse(r.decoded_bytes));
    let dicts = Dicts::load(&rows)?;
    let engines: Vec<Engine> = a
        .engines
        .iter()
        .map(|e| Engine::parse(e))
        .collect::<Result<_>>()?;
    let mut configs = Vec::new();
    for &engine in &engines {
        for &quality in &parse_qualities(&a.qualities)? {
            for &lgwin in &a.lgwins {
                for no_lcm in [false, true] {
                    if no_lcm && !a.no_lcm {
                        continue;
                    }
                    for dict in [false, true] {
                        if dict && (!a.dict || engine != Engine::C) {
                            continue;
                        }
                        configs.push(Config {
                            engine,
                            quality,
                            lgwin,
                            no_lcm,
                            dict,
                        });
                    }
                }
            }
        }
    }
    let ns = a.schedules.len();
    let nc = configs.len();
    let jobs: Vec<(usize, usize, usize)> = (0..rows.len())
        .flat_map(|r| (0..nc).flat_map(move |c| (0..ns).map(move |s| (r, c, s))))
        .filter(|&(r, c, _)| !configs[c].dict || rows[r].dictionary_sfv.is_some())
        .collect();
    eprintln!(
        "{} responses, {} configs, {} schedules: {} encodes on {} threads",
        rows.len(),
        configs.len(),
        a.schedules.len(),
        jobs.len(),
        a.threads
    );
    let out = Mutex::new(std::io::BufWriter::new(
        std::fs::File::create(&a.out).with_context(|| format!("create {}", a.out.display()))?,
    ));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let bodies: Vec<Mutex<Option<std::sync::Arc<Vec<u8>>>>> =
        rows.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..a.threads {
            sc.spawn(|| {
                let mut ctx = Ctx::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= jobs.len() || failed.lock().unwrap().is_some() {
                        return;
                    }
                    let (ri, ci, si) = jobs[i];
                    let r = one(
                        &rows[ri],
                        &bodies[ri],
                        &configs[ci],
                        &a.schedules[si],
                        &dicts,
                        &mut ctx,
                    );
                    match r {
                        Ok(v) => {
                            let mut o = out.lock().unwrap();
                            writeln!(o, "{v}").ok();
                        }
                        Err(e) => {
                            *failed.lock().unwrap() = Some(e.context(format!(
                                "{} {} {}",
                                rows[ri].url,
                                configs[ci].label(),
                                a.schedules[si]
                            )));
                            return;
                        }
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if d.is_multiple_of(500) {
                        eprintln!("  {d}/{}", jobs.len());
                    }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    out.into_inner().unwrap().flush()?;
    eprintln!("wrote {}", a.out.display());
    Ok(())
}

fn one<'d>(
    row: &Row,
    body: &Mutex<Option<std::sync::Arc<Vec<u8>>>>,
    cfg: &Config,
    sched_name: &str,
    dicts: &'d Dicts,
    ctx: &mut Ctx<'d>,
) -> Result<serde_json::Value> {
    let input = {
        let mut g = body.lock().unwrap();
        match &*g {
            Some(b) => b.clone(),
            None => {
                let b = std::sync::Arc::new(row.body()?);
                *g = Some(b.clone());
                b
            }
        }
    };
    let sched = schedule(sched_name, row)?;
    let dict = if cfg.dict {
        let p = row
            .dictionary_path()
            .ok_or_else(|| anyhow!("no dictionary"))?;
        Some((
            p,
            dicts
                .get(row)
                .ok_or_else(|| anyhow!("dictionary not loaded"))?,
        ))
    } else {
        None
    };
    let enc = ctx.encode(cfg, &input, &sched, dict.clone())?;
    let dec = brotli_c::decode(&enc, dict.map(|d| d.1)).map_err(|e| anyhow!("C decode: {e}"))?;
    if dec != *input {
        bail!("C decode differs from input");
    }
    Ok(json!({
        "group": row.group, "id": row.id, "site": row.site, "route": row.route,
        "body_sha256": row.body_sha256, "decoded": input.len(),
        "served_encoding": row.served_encoding, "served_bytes": row.served_bytes,
        "engine": cfg.engine.name(), "q": cfg.quality, "lgwin": cfg.lgwin,
        "no_lcm": cfg.no_lcm, "dict": cfg.dict,
        "schedule": sched.name, "calls": sched.cuts.len() + 1,
        // dcb framing (RFC 9842): 4-byte magic + 32-byte SHA-256.
        "size": enc.len() + if cfg.dict { 36 } else { 0 },
        "verified": "C decoder",
        "sha256": sha256_hex(&enc),
    }))
}

pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Distinct bodies only, then at most `n` per (site, size bucket), spread
/// evenly over the body hashes so the sample is deterministic.
fn sample_per_bucket(mut rows: Vec<Row>, n: usize) -> Vec<Row> {
    rows.sort_by(|x, y| x.body_sha256.cmp(&y.body_sha256));
    rows.dedup_by(|x, y| x.body_sha256 == y.body_sha256);
    let mut groups: std::collections::BTreeMap<(String, &'static str), Vec<Row>> =
        Default::default();
    for r in rows {
        groups
            .entry((r.site.clone(), corpus::size_bucket(r.decoded_bytes)))
            .or_default()
            .push(r);
    }
    let mut out = Vec::new();
    for (_, g) in groups {
        if g.len() <= n {
            out.extend(g);
        } else {
            let step = g.len() as f64 / n as f64;
            out.extend((0..n).map(|i| g[(i as f64 * step) as usize].clone()));
        }
    }
    out
}
