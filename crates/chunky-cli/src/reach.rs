//! `chunky reach`: each quality's effective match reach (RE-002, D-016), for
//! both reference encoders, buffered and streamed.
//!
//! Black-box probes on incompressible random data, so any saving is a match:
//! - `calls`: one 8,000 B block fed three times as separate calls (FLUSH
//!   between calls, or PROCESS without flushes) vs once as a single feed.
//! - `reach`: a random blob of S bytes repeated twice. A repeat is "reached"
//!   when the output is under 1.1 × S. Buffered feeds the 2S bytes in one
//!   call; streamed feeds them as 4 KiB calls with a FLUSH after each.

use anyhow::Result;

use crate::encoders::{Config, Ctx, Engine, Schedule};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "0-11")]
    qualities: String,
    #[arg(long, default_value_t = 22)]
    lgwin: u32,
}

fn random(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

pub fn run(a: Args) -> Result<()> {
    let qs = crate::baseline::parse_qualities(&a.qualities)?;
    let block = random(8000, 0x1234_5678_9abc_def1);
    let triple: Vec<u8> = [&block[..], &block[..], &block[..]].concat();
    let sizes: Vec<usize> = [
        16usize << 10,
        64 << 10,
        256 << 10,
        1 << 20,
        2 << 20,
        3 << 20,
    ]
    .into_iter()
    .filter(|&s| 2 * s < (1usize << a.lgwin) - 16)
    .collect();
    let mut ctx = Ctx::new();
    println!(
        "Effective match reach, lgwin {}. calls: 3 × 8,000 B random block (FLUSH / PROCESS / one feed); \
reach: largest S of {:?} whose repeat matched (buffered / 4 KiB calls with FLUSH).\n",
        a.lgwin,
        sizes.iter().map(|s| s >> 10).collect::<Vec<_>>()
    );
    println!(
        "| Engine | Quality | 3 calls, FLUSH | 3 calls, PROCESS | 1 feed | Reach buffered | Reach streamed |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- |");
    for engine in [Engine::C, Engine::Rust] {
        for &q in &qs {
            let cfg = Config {
                engine,
                quality: q,
                lgwin: a.lgwin,
                no_lcm: false,
                dict: false,
            };
            let cuts = vec![8000, 16000];
            let f = ctx.encode(
                &cfg,
                &triple,
                &Schedule {
                    name: "x",
                    cuts: cuts.clone(),
                    flush: true,
                },
                None,
            )?;
            let p = ctx.encode(
                &cfg,
                &triple,
                &Schedule {
                    name: "x",
                    cuts,
                    flush: false,
                },
                None,
            )?;
            let one = ctx.encode(
                &cfg,
                &triple,
                &Schedule {
                    name: "x",
                    cuts: vec![],
                    flush: false,
                },
                None,
            )?;
            let mut reach_b = 0;
            let mut reach_s = 0;
            for &s in &sizes {
                let blob = random(s, s as u64 ^ 0xfeed);
                let two = [&blob[..], &blob[..]].concat();
                let b = ctx.encode(
                    &cfg,
                    &two,
                    &Schedule {
                        name: "x",
                        cuts: vec![],
                        flush: false,
                    },
                    None,
                )?;
                if (b.len() as f64) < 1.1 * s as f64 {
                    reach_b = s;
                }
                let cuts = (1..)
                    .map(|i| i * 4096)
                    .take_while(|&c| c < two.len())
                    .collect();
                let st = ctx.encode(
                    &cfg,
                    &two,
                    &Schedule {
                        name: "x",
                        cuts,
                        flush: true,
                    },
                    None,
                )?;
                if (st.len() as f64) < 1.1 * s as f64 {
                    reach_s = s;
                }
            }
            println!(
                "| {} | q{q} | {} | {} | {} | {} | {} |",
                engine.name(),
                f.len(),
                p.len(),
                one.len(),
                kib(reach_b),
                kib(reach_s)
            );
        }
    }
    Ok(())
}

fn kib(n: usize) -> String {
    if n == 0 {
        "none".into()
    } else {
        format!("≥ {} KiB", n >> 10)
    }
}
