//! `chunky funnel`: the repeatability funnel (M1). Measures how much of each
//! response is reusable from earlier responses of the same site, in
//! chronological order. This is reusable-parse potential, not a legal
//! ordinary-br size ceiling: plain br copies can only reach the response's
//! own history (D-015, D-017).
//!
//! Stages, each as % of decoded bytes of the measured responses:
//! 1. LZ77 potential: bytes inside matches of at least 64 B into any earlier
//!    response, found from content-defined anchors (1 in 8 positions) and
//!    extended byte by byte. Approximates a cross-response dictionary.
//! 2. CDC chunk reuse: Gear content-defined chunks at 64/128/256 B average,
//!    and 256 B with delimiter bias (a cut moves to just after the next `>`
//!    or newline within 32 bytes), counted when an identical chunk occurred
//!    in an earlier response (byte-compared, not just hashed).
//! 3. Extension: hit chunks extended forward and backward into neighbouring
//!    non-hit bytes against the bytes around the earlier occurrence.
//!
//! The cut rules are fixed in code before any evaluation (no tuning), so they
//! are trivially training-only and frozen. Chunks overlapping an excluded
//! span are never admitted to the warm set (D-017, D-022); matches never
//! count excluded bytes. Rows are measured in capture order; "prior
//! sessions" admits only responses from earlier sessions, "all earlier"
//! also earlier responses of the same session.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::corpus::{self, Row};
use crate::summarize::fmt_int;

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
}

const ANCHOR_WIN: usize = 32;
const MIN_MATCH: usize = 64;

#[derive(Clone, Copy)]
pub struct Cdc {
    pub name: &'static str,
    avg_bits: u32,
    delim: bool,
}

pub const CDCS: [Cdc; 4] = [
    Cdc {
        name: "CDC 64 B",
        avg_bits: 6,
        delim: false,
    },
    Cdc {
        name: "CDC 128 B",
        avg_bits: 7,
        delim: false,
    },
    Cdc {
        name: "CDC 256 B",
        avg_bits: 8,
        delim: false,
    },
    Cdc {
        name: "CDC 256 B + delimiters",
        avg_bits: 8,
        delim: true,
    },
];

pub fn gear() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut s = 0x2545_f491_4f6c_dd1du64;
    for x in t.iter_mut() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *x = s;
    }
    t
}

/// Chunk boundaries (end offsets) for `b`.
pub fn chunks(b: &[u8], c: Cdc, g: &[u64; 256]) -> Vec<usize> {
    let avg = 1usize << c.avg_bits;
    let (min, max) = (avg / 4, avg * 4);
    let mask = (1u64 << c.avg_bits) - 1;
    let mut ends = Vec::new();
    let mut start = 0;
    let mut h = 0u64;
    let mut i = 0;
    while i < b.len() {
        h = (h << 1).wrapping_add(g[b[i] as usize]);
        i += 1;
        let len = i - start;
        let cut = (len >= min && (h >> 40) & mask == 0) || len >= max;
        if cut {
            let mut e = i;
            if c.delim
                && len < max
                && let Some(k) = b[i..b.len().min(i + 32)]
                    .iter()
                    .position(|&x| x == b'>' || x == b'\n')
            {
                e = i + k + 1;
            }
            ends.push(e);
            start = e;
            i = e;
            h = 0;
        }
    }
    if start < b.len() {
        ends.push(b.len());
    }
    ends
}

pub fn hash(b: &[u8]) -> u64 {
    // FNV-1a; equality is always confirmed byte by byte.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn mask_excluded(len: usize, spans: &[(usize, usize)]) -> Vec<bool> {
    let mut m = vec![false; len];
    for &(a, b) in spans {
        for x in m.iter_mut().take(b.min(len)).skip(a) {
            *x = true;
        }
    }
    m
}

struct Body {
    bytes: Vec<u8>,
    excluded: Vec<bool>,
    session: String,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    lz: u64,
    cdc: [u64; 4],
    ext: [u64; 4],
}

pub fn run(a: Args) -> Result<()> {
    let mut rows: Vec<Row> = corpus::load(&a.dirs)?;
    rows.sort_by(|x, y| x.captured.cmp(&y.captured));
    let sites: std::collections::BTreeSet<String> = rows.iter().map(|r| r.site.clone()).collect();
    let g = gear();
    println!(
        "Repeatability funnel (reusable-parse potential, not a br size ceiling). % of decoded bytes of \
the measured responses; excluded spans never admitted or counted.\n"
    );
    print!("| Site | Warm set | Responses | Bytes | LZ77 ≥{MIN_MATCH} B |");
    for c in CDCS {
        print!(" {} | + extension |", c.name);
    }
    println!();
    println!(
        "| --- | --- | --- | --- | --- |{}",
        " --- | --- |".repeat(CDCS.len())
    );
    for site in &sites {
        let srows: Vec<&Row> = rows.iter().filter(|r| &r.site == site).collect();
        let bodies: Vec<Body> = srows
            .iter()
            .map(|r| {
                let bytes = r.body()?;
                let excluded = mask_excluded(bytes.len(), &r.excluded);
                Ok(Body {
                    bytes,
                    excluded,
                    session: r.group.clone(),
                })
            })
            .collect::<Result<_>>()?;
        for prior_only in [false, true] {
            let first = &bodies[0].session;
            let measured: Vec<usize> = (1..bodies.len())
                .filter(|&i| !prior_only || &bodies[i].session != first)
                .collect();
            if measured.is_empty() {
                continue;
            }
            let c = measure(&bodies, &measured, prior_only, &g);
            let total: u64 = measured.iter().map(|&i| bodies[i].bytes.len() as u64).sum();
            let pct = |x: u64| format!("{:.1}%", 100.0 * x as f64 / total as f64);
            print!(
                "| {site} | {} | {} | {} | {} |",
                if prior_only {
                    "prior sessions"
                } else {
                    "all earlier"
                },
                measured.len(),
                fmt_int(total),
                pct(c.lz)
            );
            for k in 0..CDCS.len() {
                print!(" {} | {} |", pct(c.cdc[k]), pct(c.cdc[k] + c.ext[k]));
            }
            println!();
        }
    }
    Ok(())
}

/// Measure `measured` responses (indices into `bodies`, capture order).
fn measure(bodies: &[Body], measured: &[usize], prior_only: bool, g: &[u64; 256]) -> Counts {
    let mut out = Counts::default();
    let admissible =
        |j: usize, i: usize| j < i && (!prior_only || bodies[j].session != bodies[i].session);
    // Stage 1: anchor index over all bodies; lookups filtered by admissibility.
    let mut anchors: HashMap<u64, Vec<(u32, u32)>> = HashMap::new();
    for (j, b) in bodies.iter().enumerate() {
        for p in 0..b.bytes.len().saturating_sub(ANCHOR_WIN) {
            let h = hash(&b.bytes[p..p + ANCHOR_WIN]);
            if h & 7 == 0 && !b.excluded[p..p + ANCHOR_WIN].iter().any(|&x| x) {
                anchors.entry(h).or_default().push((j as u32, p as u32));
            }
        }
    }
    for &i in measured {
        let b = &bodies[i];
        let mut cov = vec![false; b.bytes.len()];
        let mut p = 0;
        while p + ANCHOR_WIN <= b.bytes.len() {
            let h = hash(&b.bytes[p..p + ANCHOR_WIN]);
            let mut best = (0, 0);
            if h & 7 == 0
                && !cov[p]
                && let Some(v) = anchors.get(&h)
            {
                for &(j, q) in v
                    .iter()
                    .filter(|(j, _)| admissible(*j as usize, i))
                    .rev()
                    .take(16)
                {
                    let o = &bodies[j as usize];
                    let q = q as usize;
                    let mut f = 0;
                    while p + f < b.bytes.len()
                        && q + f < o.bytes.len()
                        && b.bytes[p + f] == o.bytes[q + f]
                        && !b.excluded[p + f]
                        && !o.excluded[q + f]
                    {
                        f += 1;
                    }
                    let mut k = 0;
                    while k < p
                        && k < q
                        && b.bytes[p - k - 1] == o.bytes[q - k - 1]
                        && !b.excluded[p - k - 1]
                        && !o.excluded[q - k - 1]
                    {
                        k += 1;
                    }
                    if f + k > best.0 + best.1 {
                        best = (k, f);
                    }
                }
            }
            if best.0 + best.1 >= MIN_MATCH {
                for x in cov.iter_mut().take(p + best.1).skip(p - best.0) {
                    *x = true;
                }
                p += best.1.max(1);
            } else {
                p += 1;
            }
        }
        out.lz += cov.iter().filter(|&&x| x).count() as u64;
    }
    // Stages 2–3 per CDC config.
    for (k, c) in CDCS.iter().enumerate() {
        let ends: Vec<Vec<usize>> = bodies.iter().map(|b| chunks(&b.bytes, *c, g)).collect();
        // hash -> occurrences (body, start, end), admitted chunks only.
        let mut warm: HashMap<u64, Vec<(u32, u32, u32)>> = HashMap::new();
        let mut admitted_upto = 0;
        for &i in measured {
            // Admit every body before i (the warm set grows in capture order).
            while admitted_upto < i {
                let j = admitted_upto;
                let mut s = 0;
                for &e in &ends[j] {
                    if !overlaps_mask(&bodies[j].excluded, s, e) {
                        warm.entry(hash(&bodies[j].bytes[s..e]))
                            .or_default()
                            .push((j as u32, s as u32, e as u32));
                    }
                    s = e;
                }
                admitted_upto += 1;
            }
            let b = &bodies[i];
            let mut hit = vec![false; b.bytes.len()];
            let mut src: Vec<Option<(usize, usize, usize)>> = Vec::new();
            let mut s = 0;
            for &e in &ends[i] {
                let piece = &b.bytes[s..e];
                let found = warm.get(&hash(piece)).and_then(|v| {
                    v.iter().rev().find(|&&(j, a, z)| {
                        admissible(j as usize, i)
                            && &bodies[j as usize].bytes[a as usize..z as usize] == piece
                    })
                });
                if let Some(&(j, a, _)) = found {
                    if !overlaps_mask(&b.excluded, s, e) {
                        hit[s..e].iter_mut().for_each(|x| *x = true);
                        src.push(Some((s, j as usize, a as usize)));
                    } else {
                        src.push(None);
                    }
                } else {
                    src.push(None);
                }
                s = e;
            }
            out.cdc[k] += hit.iter().filter(|&&x| x).count() as u64;
            // Extension around each hit chunk.
            let mut ext = vec![false; b.bytes.len()];
            let mut s = 0;
            for (ci, &e) in ends[i].iter().enumerate() {
                if let Some((_, j, a)) = src[ci] {
                    let o = &bodies[j];
                    let len = e - s;
                    let mut f = 0;
                    while e + f < b.bytes.len()
                        && a + len + f < o.bytes.len()
                        && !hit[e + f]
                        && b.bytes[e + f] == o.bytes[a + len + f]
                        && !b.excluded[e + f]
                        && !o.excluded[a + len + f]
                    {
                        ext[e + f] = true;
                        f += 1;
                    }
                    let mut r = 0;
                    while r < s
                        && r < a
                        && !hit[s - r - 1]
                        && b.bytes[s - r - 1] == o.bytes[a - r - 1]
                        && !b.excluded[s - r - 1]
                        && !o.excluded[a - r - 1]
                    {
                        ext[s - r - 1] = true;
                        r += 1;
                    }
                }
                s = e;
            }
            out.ext[k] += ext.iter().filter(|&&x| x).count() as u64;
        }
    }
    out
}

fn overlaps_mask(m: &[bool], s: usize, e: usize) -> bool {
    m[s..e].iter().any(|&x| x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_is_deterministic_and_covers_input() {
        let g = gear();
        let b: Vec<u8> = (0..50_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        for c in CDCS {
            let e = chunks(&b, c, &g);
            assert_eq!(*e.last().unwrap(), b.len());
            assert!(e.windows(2).all(|w| w[0] < w[1]));
            assert_eq!(e, chunks(&b, c, &g));
            let avg = b.len() / e.len();
            assert!(
                avg > (1 << c.avg_bits) / 4 && avg < (1 << c.avg_bits) * 4,
                "{} avg {avg}",
                c.name
            );
        }
    }

    #[test]
    fn shifted_content_still_hits() {
        let g = gear();
        let base: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
            .collect();
        let mut shifted = b"PREFIX-INSERTED".to_vec();
        shifted.extend_from_slice(&base);
        let mk = |bytes: Vec<u8>, s: &str| Body {
            excluded: vec![false; bytes.len()],
            bytes,
            session: s.into(),
        };
        let bodies = vec![mk(base, "a"), mk(shifted, "b")];
        let c = measure(&bodies, &[1], false, &g);
        let n = bodies[1].bytes.len() as u64;
        assert!(c.lz > n * 9 / 10);
        assert!(c.cdc[2] > n * 8 / 10);
    }
}
