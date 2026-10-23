//! The reference encoders (pinned C google/brotli and rust-brotli at the fork
//! base) behind one interface, driven with identical call/flush sequences
//! (D-016, RE-002).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use chunky_core::reference::{Op, ReferenceEncoder, ReferenceParams};

use crate::corpus::Row;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Engine {
    C,
    Rust,
    /// chunky's M2 stream on its cold path: q2–q9 search and tables over the
    /// declared window (`StreamOptions::cold`, D-029).
    Chunky,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::C => "c",
            Engine::Rust => "rust",
            Engine::Chunky => "chunky",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "c" => Ok(Engine::C),
            "rust" => Ok(Engine::Rust),
            "chunky" => Ok(Engine::Chunky),
            _ => Err(anyhow!("unknown engine {s:?} (c, rust, chunky)")),
        }
    }
}

/// One point on the reference curve.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Config {
    pub engine: Engine,
    pub quality: u32,
    pub lgwin: u32,
    /// Literal context modeling disabled (decomposition variant).
    pub no_lcm: bool,
    /// Attach the response's served dictionary (Shared Brotli; C only).
    pub dict: bool,
}

impl Config {
    pub fn label(&self) -> String {
        let mut s = format!("{}-q{}-w{}", self.engine.name(), self.quality, self.lgwin);
        if self.no_lcm {
            s.push_str("-nolcm");
        }
        if self.dict {
            s.push_str("-dict");
        }
        s
    }
}

/// A call sequence: cut positions (decoded offsets) and whether each cut is
/// a FLUSH (true) or a plain PROCESS call boundary (false). The last call is
/// always FINISH.
#[derive(Clone, Debug)]
pub struct Schedule {
    pub name: &'static str,
    pub cuts: Vec<usize>,
    pub flush: bool,
}

pub const SCHEDULES: &[&str] = &["buffered", "served", "reads", "reads-noflush", "every4k"];

/// Deterministic splitmix64, so synthetic schedules depend only on the body.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Synthetic origin read sizes modeled on RE-007 (Worker reads of a
/// Cloudflare-decoded origin body): 58% uniform in 1..=81 B (median ~70 B),
/// the rest full 4 KiB reads (mean ~1.75 KB, max 4,096 B).
pub fn synthetic_reads(len: usize, seed_hex: &str) -> Vec<usize> {
    let seed = u64::from_str_radix(seed_hex.get(..16).unwrap_or("0"), 16).unwrap_or(0);
    let mut m = Mix(seed);
    let mut cuts = Vec::new();
    let mut pos = 0;
    loop {
        let r = m.next();
        let n = if r % 100 < 58 {
            1 + (r >> 8) as usize % 81
        } else {
            4096
        };
        pos += n;
        if pos >= len {
            return cuts;
        }
        cuts.push(pos);
    }
}

pub fn schedule(name: &str, row: &Row) -> Result<Schedule> {
    let len = row.decoded_bytes;
    Ok(match name {
        "buffered" => Schedule {
            name: "buffered",
            cuts: vec![],
            flush: false,
        },
        "served" => Schedule {
            name: "served",
            cuts: row.flushes.iter().copied().filter(|&f| f < len).collect(),
            flush: true,
        },
        "reads" => Schedule {
            name: "reads",
            cuts: synthetic_reads(len, &row.body_sha256),
            flush: true,
        },
        "reads-noflush" => Schedule {
            name: "reads-noflush",
            cuts: synthetic_reads(len, &row.body_sha256),
            flush: false,
        },
        "every4k" => Schedule {
            name: "every4k",
            cuts: (1..).map(|i| i * 4096).take_while(|&c| c < len).collect(),
            flush: true,
        },
        _ => return Err(anyhow!("unknown schedule {name:?}; known: {SCHEDULES:?}")),
    })
}

/// Per-thread encoder context: C prepared dictionaries are built once per
/// dictionary and reused (they borrow the bytes from `Dicts`).
pub struct Ctx<'d> {
    prepared: HashMap<PathBuf, brotli_c::PreparedDictionary<'d>>,
    /// chunky's per-thread block scratch, reused across responses.
    ws: chunky_core::stream::Workspace,
}

impl<'d> Ctx<'d> {
    pub fn new() -> Self {
        Self {
            prepared: HashMap::new(),
            ws: chunky_core::stream::Workspace::default(),
        }
    }

    /// Encode `input` with `cfg` along `sched`. `dict` is the response's
    /// dictionary (path, bytes) when `cfg.dict` is set.
    pub fn encode(
        &mut self,
        cfg: &Config,
        input: &[u8],
        sched: &Schedule,
        dict: Option<(PathBuf, &'d [u8])>,
    ) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(input.len() / 3 + 1024);
        match cfg.engine {
            Engine::C => {
                let mut p = brotli_c::EncoderParams::new(cfg.quality, cfg.lgwin);
                p.disable_literal_context_modeling = cfg.no_lcm;
                let pd = match (cfg.dict, dict) {
                    (true, Some((path, bytes))) => Some(match self.prepared.entry(path) {
                        std::collections::hash_map::Entry::Occupied(e) => &*e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            &*e.insert(brotli_c::PreparedDictionary::new(bytes, 11)?)
                        }
                    }),
                    (true, None) => return Err(anyhow!("dictionary config without dictionary")),
                    _ => None,
                };
                let mut e = brotli_c::Encoder::new(&p)?;
                if let Some(d) = pd {
                    e.attach(d)?;
                }
                drive(input, sched, |chunk, op| {
                    let op = match op {
                        Op::Process => brotli_c::Op::Process,
                        Op::Flush => brotli_c::Op::Flush,
                        Op::Finish => brotli_c::Op::Finish,
                    };
                    e.push(chunk, op, &mut out).map_err(|e| anyhow!("{e}"))
                })?;
            }
            Engine::Rust => {
                if cfg.dict {
                    return Err(anyhow!(
                        "rust-brotli has no Shared Brotli dictionary API (RE-004)"
                    ));
                }
                let mut p = ReferenceParams::new(cfg.quality, cfg.lgwin);
                p.disable_literal_context_modeling = cfg.no_lcm;
                let mut e = ReferenceEncoder::new(&p)?;
                drive(input, sched, |chunk, op| {
                    e.push(chunk, op, &mut out).map_err(|e| anyhow!("{e}"))
                })?;
            }
            Engine::Chunky => {
                if cfg.dict || cfg.no_lcm {
                    return Err(anyhow!("chunky configs take no dictionary or no-lcm"));
                }
                let engine = chunky_engine(cfg)?;
                let mut s = engine.stream();
                drive(input, sched, |chunk, op| {
                    s.feed(&mut self.ws, chunk, &mut out)?;
                    match op {
                        Op::Process => Ok(()),
                        Op::Flush => s.flush(&mut self.ws, &mut out),
                        Op::Finish => s.finish(&mut self.ws, &mut out),
                    }
                    .map_err(|e| anyhow!("{e}"))
                })?;
            }
        }
        Ok(out)
    }
}

/// The cold-path engine for a chunky config (q2..=9).
pub fn chunky_engine(cfg: &Config) -> Result<chunky_core::stream::Engine> {
    use chunky_core::stream::StreamOptions;
    let opts = StreamOptions {
        table_quality: cfg.quality,
        search_quality: cfg.quality,
        ..StreamOptions::cold(cfg.lgwin)
    };
    chunky_core::stream::Engine::new(opts).map_err(|e| anyhow!("{}: {e}", cfg.label()))
}

/// Issue the schedule's calls: one call per segment, FLUSH or PROCESS at each
/// cut, FINISH on the tail.
pub fn drive(
    input: &[u8],
    sched: &Schedule,
    mut push: impl FnMut(&[u8], Op) -> Result<()>,
) -> Result<()> {
    let mut pos = 0;
    let op = if sched.flush { Op::Flush } else { Op::Process };
    for &c in &sched.cuts {
        let c = c.min(input.len());
        if c > pos {
            push(&input[pos..c], op)?;
            pos = c;
        }
    }
    push(&input[pos..], Op::Finish)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_reads_match_re007_shape() {
        let cuts = synthetic_reads(534_000, "0123456789abcdef");
        let mut sizes: Vec<usize> = std::iter::once(cuts[0])
            .chain(cuts.windows(2).map(|w| w[1] - w[0]))
            .collect();
        sizes.sort_unstable();
        let median = sizes[sizes.len() / 2];
        assert!((40..=120).contains(&median), "median {median}");
        assert!((200..=420).contains(&sizes.len()), "{} reads", sizes.len());
        assert_eq!(*sizes.last().unwrap(), 4096);
        assert_eq!(cuts, synthetic_reads(534_000, "0123456789abcdef"));
    }

    #[test]
    fn engines_agree_on_decoded_bytes_for_every_schedule() {
        let input: Vec<u8> = (0..60_000u32)
            .flat_map(|i| format!("<a href=\"/x/{}\">{}</a>", i % 89, i % 13).into_bytes())
            .take(60_000)
            .collect();
        let row = Row {
            dir: PathBuf::new(),
            group: String::new(),
            id: String::new(),
            site: String::new(),
            route: String::new(),
            url: String::new(),
            body_sha256: "00ff00ff00ff00ff".into(),
            decoded_bytes: input.len(),
            served_encoding: None,
            served_bytes: None,
            flushes: vec![15, 9000, 30_000],
            dictionary_sfv: None,
            flags: vec![],
            captured: String::new(),
            excluded: vec![],
            split: String::new(),
        };
        let mut ctx = Ctx::new();
        for name in SCHEDULES {
            let s = schedule(name, &row).unwrap();
            for engine in [Engine::C, Engine::Rust, Engine::Chunky] {
                for q in [1, 2, 5, 11] {
                    if engine == Engine::Chunky && !(2..=9).contains(&q) {
                        continue;
                    }
                    let cfg = Config {
                        engine,
                        quality: q,
                        lgwin: 22,
                        no_lcm: false,
                        dict: false,
                    };
                    let out = ctx.encode(&cfg, &input, &s, None).unwrap();
                    assert_eq!(
                        brotli_c::decode(&out, None).unwrap(),
                        input,
                        "{name} {cfg:?}"
                    );
                }
            }
        }
    }
}
