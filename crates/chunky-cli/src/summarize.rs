//! `chunky summarize`: markdown tables from `chunky baseline` JSONL.
//!
//! Per site (and per group, if asked), totals each config along each
//! schedule over the same set of responses, as bytes and as a ratio to the
//! reference point (C q11 buffered at the same window unless overridden).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    /// JSONL files from `chunky baseline`.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Reference config label for ratios.
    #[arg(long, default_value = "c-q11-w22")]
    reference: String,
    /// Reference schedule for ratios.
    #[arg(long, default_value = "buffered")]
    reference_schedule: String,
    /// Split tables by group (session/slice) as well as site.
    #[arg(long)]
    by_group: bool,
    /// Split tables by route as well (size bucket for multi-origin rows).
    #[arg(long)]
    by_route: bool,
}

#[derive(Default, Clone, Copy)]
struct Acc {
    size: u64,
    n: u64,
}

pub fn run(a: Args) -> Result<()> {
    // (table, config, schedule) -> response -> size. Keyed per response so
    // overlapping inputs (e.g. a --no-lcm run that repeats the plain configs)
    // count each response once.
    let mut per: BTreeMap<(String, String, String), BTreeMap<String, u64>> = BTreeMap::new();
    let mut decoded: BTreeMap<(String, String), BTreeMap<String, u64>> = BTreeMap::new();
    let mut served: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for p in &a.inputs {
        let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
        for line in text.lines().filter(|l| !l.is_empty()) {
            let v: Value = serde_json::from_str(line)?;
            let g = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            let mut label = format!("{}-q{}-w{}", g("engine"), n("q"), n("lgwin"));
            if v.get("no_lcm").and_then(Value::as_bool) == Some(true) {
                label.push_str("-nolcm");
            }
            let dict = v.get("dict").and_then(Value::as_bool) == Some(true);
            if dict {
                label.push_str("-dict");
            }
            let mut table = g("site");
            if a.by_group {
                table = format!("{table}/{}", g("group"));
            }
            if a.by_route {
                table = format!("{table} {}", g("route"));
            }
            // dcb-served rows get a second table of their own so dictionary
            // configs compare against the same responses.
            let mut tables = Vec::new();
            if !dict {
                tables.push(table.clone());
            }
            if g("served_encoding") == "dcb" {
                tables.push(format!("{table}, dcb-served rows"));
            }
            let key = format!("{}#{}", g("group"), g("id"));
            for table in tables {
                per.entry((table.clone(), label.clone(), g("schedule")))
                    .or_default()
                    .insert(key.clone(), n("size"));
                decoded
                    .entry((table.clone(), g("body_sha256")))
                    .or_default()
                    .insert(key.clone(), n("decoded"));
                if n("served_bytes") > 0 {
                    served
                        .entry(table)
                        .or_default()
                        .insert(key.clone(), n("served_bytes"));
                }
            }
        }
    }
    let t: BTreeMap<(String, String, String), Acc> = per
        .into_iter()
        .map(|(k, m)| {
            let acc = Acc {
                size: m.values().sum(),
                n: m.len() as u64,
            };
            (k, acc)
        })
        .collect();
    let tables: BTreeSet<String> = t.keys().map(|k| k.0.clone()).collect();
    for table in tables {
        let configs: BTreeSet<String> = t
            .keys()
            .filter(|k| k.0 == table)
            .map(|k| k.1.clone())
            .collect();
        let scheds: Vec<String> = {
            let s: BTreeSet<String> = t
                .keys()
                .filter(|k| k.0 == table)
                .map(|k| k.2.clone())
                .collect();
            let order = ["buffered", "served", "reads", "reads-noflush", "every4k"];
            let mut v: Vec<String> = s.into_iter().collect();
            v.sort_by_key(|x| order.iter().position(|o| o == x).unwrap_or(99));
            v
        };
        let reference = t
            .get(&(
                table.clone(),
                a.reference.clone(),
                a.reference_schedule.clone(),
            ))
            .copied();
        let total_decoded: u64 = decoded
            .iter()
            .filter(|(k, _)| k.0 == table)
            .flat_map(|(_, m)| m.values())
            .sum();
        let responses: usize = decoded
            .iter()
            .filter(|(k, _)| k.0 == table)
            .map(|(_, m)| m.len())
            .sum();
        println!(
            "\n### {table}: {responses} responses, {} decoded bytes\n",
            fmt_int(total_decoded)
        );
        if let Some(r) = reference {
            println!(
                "Ratios are to {} {} ({} B over {} responses).\n",
                a.reference,
                a.reference_schedule,
                fmt_int(r.size),
                r.n
            );
        }
        print!("| Config |");
        for s in &scheds {
            print!(" {s} |");
        }
        println!();
        print!("| --- |");
        for _ in &scheds {
            print!(" --- |");
        }
        println!();
        let mut configs: Vec<String> = configs.into_iter().collect();
        configs.sort_by_key(|c| {
            let q: u32 = c
                .split('-')
                .find_map(|p| p.strip_prefix('q').and_then(|x| x.parse().ok()))
                .unwrap_or(0);
            (
                c.contains("-dict"),
                c.contains("nolcm"),
                c.split('-').next().unwrap_or("").to_string(),
                q,
                c.clone(),
            )
        });
        for c in &configs {
            print!("| {c} |");
            for s in &scheds {
                match t.get(&(table.clone(), c.clone(), s.clone())) {
                    Some(x) => match reference {
                        Some(r) if r.n == x.n && r.size > 0 => print!(
                            " {} ({:.3}) |",
                            fmt_int(x.size),
                            x.size as f64 / r.size as f64
                        ),
                        _ => print!(" {} [n={}] |", fmt_int(x.size), x.n),
                    },
                    None => print!(" — |"),
                }
            }
            println!();
        }
        if let Some(m) = served.get(&table) {
            let total: u64 = m.values().sum();
            match reference {
                Some(r) if r.n as usize == m.len() => println!(
                    "\nServed by the origin (as captured, incl. any dcb header): {} B ({:.3}).",
                    fmt_int(total),
                    total as f64 / r.size as f64
                ),
                _ => println!(
                    "\nServed by the origin (as captured): {} B over {} responses.",
                    fmt_int(total),
                    m.len()
                ),
            }
        }
    }
    Ok(())
}

pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
