//! `chunky generic-reuse`: held-out-origin chunk reuse for the generic tier
//! (D-026). Builds a warm set of content-defined chunks from one split's
//! origins (default train) and reports the share of another split's bytes
//! (default heldout) inside chunks already in it, per content type. Bodies
//! that are byte-identical to a warm body are reported separately, so
//! chunk-level reuse is not confused with whole-file duplicates.
//!
//! Chunks match on a 64-bit hash plus length (bodies are streamed, not
//! kept), so this is a potential estimate like the funnel, not a byte-exact
//! cache. As with the funnel, it is reusable-parse potential: plain br
//! cannot reference another origin's bytes (D-026).
//! Chunks touching excluded spans are neither admitted nor counted, including
//! in the whole-body reuse diagnostic (D-017).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::corpus::{self, Row};
use crate::funnel::{CDCS, Cdc, chunks, gear, hash};
use crate::summarize::fmt_int;

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    #[arg(long, default_value = "train")]
    warm_split: String,
    #[arg(long, default_value = "heldout")]
    measure_split: String,
    /// Warm-set sample per content type, in decoded bytes.
    #[arg(long, default_value_t = 1 << 30)]
    warm_bytes: usize,
    /// Measured sample per content type, in decoded bytes.
    #[arg(long, default_value_t = 256 << 20)]
    measure_bytes: usize,
}

/// Distinct bodies spread over the hashes, approximately `budget` bytes.
/// A positive budget selects at least one nonempty body.
fn spread(mut rows: Vec<Row>, budget: usize) -> Vec<Row> {
    rows.sort_by(|x, y| x.body_sha256.cmp(&y.body_sha256));
    rows.dedup_by(|x, y| x.body_sha256 == y.body_sha256);
    let sizes: Vec<usize> = rows.iter().map(|r| r.decoded_bytes).collect();
    corpus::spread_indices(&sizes, budget)
        .into_iter()
        .map(|i| rows[i].clone())
        .collect()
}

fn overlaps(excluded: &[(usize, usize)], start: usize, end: usize) -> bool {
    excluded.iter().any(|&(a, b)| a < end && b > start && a < b)
}

/// Like the per-site funnel, never admit or count a chunk that touches an
/// excluded span. The denominator remains the full decoded body size.
fn eligible_chunks<'a>(
    body: &'a [u8],
    excluded: &'a [(usize, usize)],
    cdc: Cdc,
    g: &[u64; 256],
) -> impl Iterator<Item = (u64, u32)> + 'a {
    let mut start = 0;
    chunks(body, cdc, g).into_iter().filter_map(move |end| {
        let s = start;
        start = end;
        (!overlaps(excluded, s, end)).then(|| (hash(&body[s..end]), (end - s) as u32))
    })
}

pub fn run(a: Args) -> Result<()> {
    let rows = corpus::load(&a.dirs)?;
    let g = gear();
    let mut by_type: BTreeMap<String, (Vec<Row>, Vec<Row>)> = BTreeMap::new();
    for r in rows {
        let e = by_type.entry(r.site.clone()).or_default();
        if r.split == a.warm_split {
            e.0.push(r);
        } else if r.split == a.measure_split {
            e.1.push(r);
        }
    }
    let idx = [0usize, 2]; // CDC 64 B and 256 B
    println!(
        "Generic-tier chunk reuse: {} origins' bytes inside chunks from {} origins (distinct bodies; \
hash+length matching).\n",
        a.measure_split, a.warm_split
    );
    print!(
        "| Type | Warm bodies | Warm bytes | Measured bodies | Measured bytes | Whole body seen |"
    );
    for &k in &idx {
        print!(" {} |", CDCS[k].name);
    }
    println!();
    println!(
        "| --- | --- | --- | --- | --- | --- |{}",
        " --- |".repeat(idx.len())
    );
    for (ty, (warm, meas)) in by_type {
        let warm = spread(warm, a.warm_bytes);
        let meas = spread(meas, a.measure_bytes);
        let warm_bodies: HashSet<String> = warm
            .iter()
            .filter(|r| !overlaps(&r.excluded, 0, r.decoded_bytes))
            .map(|r| r.body_sha256.clone())
            .collect();
        let mut sets: Vec<HashSet<(u64, u32)>> = vec![HashSet::new(); idx.len()];
        let mut warm_total = 0u64;
        for r in &warm {
            let b = r.body()?;
            warm_total += b.len() as u64;
            for (si, &k) in idx.iter().enumerate() {
                sets[si].extend(eligible_chunks(&b, &r.excluded, CDCS[k], &g));
            }
        }
        let mut total = 0u64;
        let mut whole = 0u64;
        let mut hit = vec![0u64; idx.len()];
        for r in &meas {
            let b = r.body()?;
            total += b.len() as u64;
            if warm_bodies.contains(&r.body_sha256) && !overlaps(&r.excluded, 0, b.len()) {
                whole += b.len() as u64;
            }
            for (si, &k) in idx.iter().enumerate() {
                for chunk in eligible_chunks(&b, &r.excluded, CDCS[k], &g) {
                    if sets[si].contains(&chunk) {
                        hit[si] += chunk.1 as u64;
                    }
                }
            }
        }
        let pct = |x: u64| format!("{:.1}%", 100.0 * x as f64 / total.max(1) as f64);
        print!(
            "| {ty} | {} | {} | {} | {} | {} |",
            fmt_int(warm.len() as u64),
            fmt_int(warm_total),
            fmt_int(meas.len() as u64),
            fmt_int(total),
            pct(whole)
        );
        for h in hit {
            print!(" {} |", pct(h));
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_bytes_are_neither_admitted_nor_counted() {
        let body = b"a secret token shared across two origins".repeat(100);
        let g = gear();
        for cdc in CDCS {
            let public: HashSet<_> = eligible_chunks(&body, &[], cdc, &g).collect();
            assert!(!public.is_empty());
            let excluded = [(0, body.len())];
            let secret: HashSet<_> = eligible_chunks(&body, &excluded, cdc, &g).collect();
            assert!(secret.is_empty());
            assert!(public.intersection(&secret).next().is_none());
            // Exclusion is per occurrence, including a span inside a chunk.
            let ends = chunks(&body, cdc, &g);
            let excluded = [(ends[0] - 1, ends[0])];
            let eligible_bytes: u32 = eligible_chunks(&body, &excluded, cdc, &g)
                .map(|(_, len)| len)
                .sum();
            assert_eq!(eligible_bytes as usize, body.len() - ends[0]);
        }
    }
}
