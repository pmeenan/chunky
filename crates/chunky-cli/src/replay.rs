//! `chunky replay`: explain the M2 stream encoder's output sizes on the corpus.
//!
//! For every response and schedule it encodes the reference qualities (q2,
//! q5, q11 with FLUSH at the cuts), the cold path, and the buffered q11 parse
//! injected under several table/block settings, decodes every output with
//! the C reference decoder, and reports sizes with stream statistics.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use chunky_core::reference::{ReferenceParams, capture_parse, compress_with_flushes};
use chunky_core::stream::{
    Engine, StreamOptions, StreamStats, Workspace, encode_with_flushes, flatten_parse,
};
use serde_json::{Value, json};

use crate::corpus::{self, Row};
use crate::encoders::schedule;

const LGWIN: u32 = 22;
const CONFIGS: &[&str] = &[
    "ref-q2",
    "ref-q5",
    "ref-q11",
    "cold",
    "cold-r18",
    "cold-fullring",
    "inj-oracle",
    "inj-fast",
    "inj-q5tables",
    "inj-oracle-b18",
    "replay-exact",
];

#[derive(clap::Args)]
pub struct Args {
    /// Manifest directories (sessions or slices).
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    /// JSONL output; the markdown summary goes to the same path with `.md`.
    #[arg(long)]
    out: PathBuf,
    /// Call schedules (flush schedules only): buffered, served, reads, every4k.
    #[arg(long, value_delimiter = ',', default_value = "buffered,served,reads")]
    schedules: Vec<String>,
    /// Only these sites (default all).
    #[arg(long, value_delimiter = ',')]
    sites: Vec<String>,
    /// Deterministic sample: at most this many distinct bodies per site and
    /// size bucket (0 = all).
    #[arg(long, default_value_t = 0)]
    max_per_bucket: usize,
    #[arg(long, default_value_t = default_threads())]
    threads: usize,
}

fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(2).max(1))
}

/// One output row.
struct Rec {
    row: usize,
    schedule: String,
    config: &'static str,
    size: usize,
    stats: Option<StreamStats>,
    identical: Option<bool>,
    decode_ok: bool,
}

pub fn run(a: Args) -> Result<()> {
    if a.threads == 0 {
        bail!("--threads must be greater than zero");
    }
    for s in &a.schedules {
        if s == "reads-noflush" {
            bail!("reads-noflush is not a flush schedule; replay needs flushes");
        }
    }
    let mut rows = corpus::load(&a.dirs)?;
    if !a.sites.is_empty() {
        rows.retain(|r| a.sites.contains(&r.site));
    }
    if a.max_per_bucket > 0 {
        rows = sample_per_bucket(rows, a.max_per_bucket);
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.decoded_bytes));
    eprintln!(
        "{} responses, {} schedules, {} configs on {} threads",
        rows.len(),
        a.schedules.len(),
        CONFIGS.len(),
        a.threads
    );
    let started = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let recs: Mutex<Vec<Rec>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..a.threads {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= rows.len() {
                        return;
                    }
                    match response(i, &rows[i], &a.schedules, &errors) {
                        Ok(v) => recs.lock().unwrap().extend(v),
                        Err(e) => errors
                            .lock()
                            .unwrap()
                            .push(format!("{} {}: {e:#}", rows[i].group, rows[i].id)),
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if d.is_multiple_of(10) {
                        eprintln!("  {d}/{} ({:.0?})", rows.len(), started.elapsed());
                    }
                }
            });
        }
    });
    let mut recs = recs.into_inner().unwrap();
    let errors = errors.into_inner().unwrap();
    let key = |c: &str| CONFIGS.iter().position(|x| *x == c).unwrap_or(usize::MAX);
    recs.sort_by(|x, y| {
        (x.row, &x.schedule, key(x.config)).cmp(&(y.row, &y.schedule, key(y.config)))
    });
    let mut o = std::io::BufWriter::new(
        std::fs::File::create(&a.out).with_context(|| format!("create {}", a.out.display()))?,
    );
    for r in &recs {
        let row = &rows[r.row];
        let st = r.stats.as_ref();
        let sv = |f: fn(&StreamStats) -> u64| st.map_or(Value::Null, |s| json!(f(s)));
        writeln!(
            o,
            "{}",
            json!({
                "group": row.group, "id": row.id, "site": row.site, "route": row.route,
                "decoded_bytes": row.decoded_bytes, "schedule": r.schedule,
                "config": r.config, "size": r.size,
                "blocks": sv(|s| u64::from(s.blocks)),
                "paddings": sv(|s| u64::from(s.paddings)),
                "rejected_copies": sv(|s| s.rejected_copies),
                "rejected_bytes": sv(|s| s.rejected_bytes),
                "uncovered_bytes": sv(|s| s.uncovered_bytes),
                "max_distance": sv(|s| u64::from(s.max_distance)),
                "identical_to_ref": r.identical,
                "decode_ok": r.decode_ok,
            })
        )?;
    }
    o.flush()?;
    let md = summary(&rows, &recs, &a.schedules, &errors, started.elapsed());
    let mdp = a.out.with_extension("md");
    std::fs::write(&mdp, &md).with_context(|| format!("write {}", mdp.display()))?;
    println!("{md}");
    eprintln!("wrote {} and {}", a.out.display(), mdp.display());
    if !errors.is_empty() {
        bail!("{} replay errors; see {}", errors.len(), mdp.display());
    }
    Ok(())
}

/// Every configuration along every schedule for one response.
fn response(
    ri: usize,
    row: &Row,
    schedules: &[String],
    errors: &Mutex<Vec<String>>,
) -> Result<Vec<Rec>> {
    let body = row.body()?;
    let (q11_buffered, blocks) = capture_parse(&ReferenceParams::new(11, LGWIN), &body, &[])
        .map_err(|e| anyhow!("capture_parse: {e}"))?;
    let parse = flatten_parse(&blocks);
    let largest = blocks.iter().map(|b| b.len).max().unwrap_or(1);
    let layout_bits = (32 - (largest.max(1) - 1).leading_zeros()).max(10);
    let mut out = Vec::new();
    let fail = |what: &str, sched: &str, e: String| {
        errors
            .lock()
            .unwrap()
            .push(format!("{} {} {sched} {what}: {e}", row.group, row.id));
    };
    for sname in schedules {
        let sched = schedule(sname, row)?;
        let cuts = &sched.cuts;
        let mut push = |config: &'static str,
                        bytes: Vec<u8>,
                        stats: Option<StreamStats>,
                        ident: Option<bool>| {
            let decode_ok = match brotli_c::decode(&bytes, None) {
                Ok(d) => d == body,
                Err(_) => false,
            };
            if !decode_ok {
                errors.lock().unwrap().push(format!(
                    "DECODE FAILURE {} {} {} {config}",
                    row.group, row.id, sched.name
                ));
            }
            out.push(Rec {
                row: ri,
                schedule: sched.name.to_string(),
                config,
                size: bytes.len(),
                stats,
                identical: ident,
                decode_ok,
            });
        };
        let mut ref_q11: Option<Vec<u8>> = None;
        for (name, q) in [("ref-q2", 2), ("ref-q5", 5), ("ref-q11", 11)] {
            if q == 11 && cuts.is_empty() {
                // Buffered q11 was already produced by the capture run.
                push(name, q11_buffered.clone(), None, None);
                ref_q11 = Some(q11_buffered.clone());
                continue;
            }
            match compress_with_flushes(&ReferenceParams::new(q, LGWIN), &body, cuts) {
                Ok(b) => {
                    if q == 11 {
                        ref_q11 = Some(b.clone());
                    }
                    push(name, b, None, None)
                }
                Err(e) => fail(name, sched.name, e.to_string()),
            }
        }
        let cold = StreamOptions::cold(LGWIN);
        let inj = |tq: u32, block_bits: u32| StreamOptions {
            block_bits,
            ..StreamOptions::external(LGWIN, tq)
        };
        let variants: [(&'static str, StreamOptions, bool); 7] = [
            ("cold", cold, false),
            (
                "cold-r18",
                StreamOptions {
                    retained_bits: 18,
                    ..cold
                },
                false,
            ),
            (
                "cold-fullring",
                StreamOptions {
                    compact_ring: false,
                    ..cold
                },
                false,
            ),
            ("inj-oracle", inj(11, 16), true),
            ("inj-fast", inj(2, 16), true),
            ("inj-q5tables", inj(5, 16), true),
            ("inj-oracle-b18", inj(11, 18), true),
        ];
        for (name, opts, injected) in variants {
            let r = Engine::new(opts).and_then(|e| {
                encode_with_flushes(
                    &e,
                    &mut Workspace::default(),
                    injected.then(|| parse.clone()),
                    &body,
                    cuts,
                )
            });
            match r {
                Ok((b, st)) => push(name, b, Some(st), None),
                Err(e) => fail(name, sched.name, e.to_string()),
            }
        }
        if sched.name == "buffered" {
            // The reference's own ring layout: its blocks can reach 2^22.
            let opts = StreamOptions {
                retained_bits: 22,
                block_bits: layout_bits,
                compact_ring: false,
                ..StreamOptions::external(LGWIN, 11)
            };
            let r = replay_exact(opts, &parse, &blocks, &body);
            match r {
                Ok((b, st)) => {
                    let same = ref_q11.as_deref() == Some(b.as_slice());
                    push("replay-exact", b, Some(st), Some(same));
                }
                Err(e) => fail("replay-exact", sched.name, e),
            }
        }
    }
    Ok(out)
}

/// The captured parse on its own meta-block layout.
fn replay_exact(
    opts: StreamOptions,
    parse: &[chunky_core::stream::ExternalCommand],
    blocks: &[chunky_core::stream::CapturedBlock],
    body: &[u8],
) -> Result<(Vec<u8>, StreamStats), String> {
    let e = Engine::new(opts).map_err(|e| e.to_string())?;
    let mut ws = Workspace::default();
    let mut s = e.stream_with_parse(parse.to_vec());
    let mut out = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let start = b.start as usize;
        s.feed(&mut ws, &body[start..start + b.len as usize], &mut out)
            .map_err(|e| e.to_string())?;
        if i + 1 < blocks.len() {
            s.cut(&mut ws, &mut out).map_err(|e| e.to_string())?;
        }
    }
    s.finish(&mut ws, &mut out).map_err(|e| e.to_string())?;
    Ok((out, *s.stats()))
}

#[derive(Default, Clone)]
struct Agg {
    n: usize,
    bytes: u64,
    blocks: u64,
    paddings: u64,
    rejected: u64,
    uncovered: u64,
    max_dist: u32,
    ident: usize,
    ident_n: usize,
    bad_decode: usize,
}

fn summary(
    rows: &[Row],
    recs: &[Rec],
    schedules: &[String],
    errors: &[String],
    elapsed: std::time::Duration,
) -> String {
    let mut sites: Vec<String> = rows.iter().map(|r| r.site.clone()).collect();
    sites.sort();
    sites.dedup();
    let mut groups: Vec<(String, Option<&str>)> = sites
        .iter()
        .map(|s| (s.clone(), Some(s.as_str())))
        .collect();
    if sites.len() > 1 {
        groups.push(("all sites".into(), None));
    }
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# chunky replay (lgwin {LGWIN})\n\n{} responses, {} decode/encode errors, wall time {:.0?}.\n",
        rows.len(),
        errors.len(),
        elapsed
    );
    // (site group, schedule, config) -> Agg
    let mut tot_rejected = 0u64;
    let mut bad_decode = 0usize;
    for r in recs {
        tot_rejected += r.stats.map_or(0, |s| s.rejected_copies);
        bad_decode += usize::from(!r.decode_ok);
    }
    let _ = writeln!(
        md,
        "Decode failures (C reference decoder): {bad_decode}. Total rejected copies across all rows: {tot_rejected}.\n"
    );
    for (label, filter) in &groups {
        let mut agg: BTreeMap<(String, &str), Agg> = BTreeMap::new();
        let mut nresp = 0;
        for row in rows {
            if filter.is_some_and(|f| f != row.site) {
                continue;
            }
            nresp += 1;
        }
        for r in recs {
            if filter.is_some_and(|f| f != rows[r.row].site) {
                continue;
            }
            let a = agg.entry((r.schedule.clone(), r.config)).or_default();
            a.n += 1;
            a.bytes += r.size as u64;
            a.bad_decode += usize::from(!r.decode_ok);
            if let Some(s) = &r.stats {
                a.blocks += u64::from(s.blocks);
                a.paddings += u64::from(s.paddings);
                a.rejected += s.rejected_copies;
                a.uncovered += s.uncovered_bytes;
                a.max_dist = a.max_dist.max(s.max_distance);
            }
            if let Some(i) = r.identical {
                a.ident_n += 1;
                a.ident += usize::from(i);
            }
        }
        let q11_buffered = agg
            .get(&("buffered".to_string(), "ref-q11"))
            .map_or(0, |a| a.bytes);
        let decoded: u64 = rows
            .iter()
            .filter(|r| filter.is_none_or(|f| f == r.site))
            .map(|r| r.decoded_bytes as u64)
            .sum();
        let _ = writeln!(
            md,
            "## {label}: {nresp} responses, {decoded} decoded bytes\n"
        );
        for sname in schedules {
            let q11 = agg.get(&(sname.clone(), "ref-q11")).map_or(0, |a| a.bytes);
            let _ = writeln!(md, "### {label}, schedule `{sname}`\n");
            let _ = writeln!(
                md,
                "| config | n | total bytes | vs ref-q11 ({sname}) | vs ref-q11 buffered | blocks | paddings | rejected copies | uncovered B | max distance | identical to ref | decode failures |"
            );
            let _ = writeln!(
                md,
                "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
            );
            for &c in CONFIGS {
                let Some(a) = agg.get(&(sname.clone(), c)) else {
                    continue;
                };
                let ratio = |d: u64| {
                    if d == 0 {
                        "n/a".to_string()
                    } else {
                        format!("{:.4}", a.bytes as f64 / d as f64)
                    }
                };
                let is_ref = c.starts_with("ref-");
                let stat = |v: u64| {
                    if is_ref {
                        "-".to_string()
                    } else {
                        v.to_string()
                    }
                };
                let ident = if a.ident_n > 0 {
                    format!("{} of {}", a.ident, a.ident_n)
                } else {
                    "-".into()
                };
                let _ = writeln!(
                    md,
                    "| {c} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {ident} | {} |",
                    a.n,
                    a.bytes,
                    ratio(q11),
                    ratio(q11_buffered),
                    stat(a.blocks),
                    stat(a.paddings),
                    stat(a.rejected),
                    stat(a.uncovered),
                    stat(u64::from(a.max_dist)),
                    a.bad_decode
                );
            }
            let _ = writeln!(md);
        }
    }
    // Non-identical exact replays by response.
    let nonident: Vec<String> = recs
        .iter()
        .filter(|r| r.identical == Some(false))
        .map(|r| format!("{}/{}", rows[r.row].group, rows[r.row].id))
        .collect();
    let _ = writeln!(md, "## Non-identical exact replays\n");
    if nonident.is_empty() {
        let _ = writeln!(md, "None.\n");
    } else {
        for n in &nonident {
            let _ = writeln!(md, "- {n}");
        }
        let _ = writeln!(md);
    }
    let rej: Vec<String> = recs
        .iter()
        .filter(|r| r.stats.is_some_and(|s| s.rejected_copies > 0))
        .map(|r| {
            format!(
                "{}/{} {} {}",
                rows[r.row].group, rows[r.row].id, r.schedule, r.config
            )
        })
        .collect();
    let _ = writeln!(md, "## Rows with rejected copies\n");
    if rej.is_empty() {
        let _ = writeln!(md, "None.\n");
    } else {
        for n in &rej {
            let _ = writeln!(md, "- {n}");
        }
        let _ = writeln!(md);
    }
    let _ = writeln!(md, "## Errors\n");
    if errors.is_empty() {
        let _ = writeln!(md, "None.");
    } else {
        for e in errors {
            let _ = writeln!(md, "- {e}");
        }
    }
    md
}

/// Distinct bodies only, then at most `n` per (site, size bucket), spread
/// evenly over the body hashes so the sample is deterministic.
fn sample_per_bucket(mut rows: Vec<Row>, n: usize) -> Vec<Row> {
    rows.sort_by(|x, y| x.body_sha256.cmp(&y.body_sha256));
    rows.dedup_by(|x, y| x.body_sha256 == y.body_sha256);
    let mut groups: BTreeMap<(String, &'static str), Vec<Row>> = BTreeMap::new();
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
