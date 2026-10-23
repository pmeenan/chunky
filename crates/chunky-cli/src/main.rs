//! `chunky`: corpus, baseline and measurement tools (native only).

mod alloc_count;
mod baseline;
mod bench;
mod corpus;
mod encoders;
mod flushes;
mod funnel;
mod generic_reuse;
mod identity;
mod inspect;
mod mem;
mod parse_stats;
mod provenance;
mod reach;
mod replay;
mod summarize;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[global_allocator]
static ALLOC: alloc_count::Counting = alloc_count::Counting;

#[derive(Parser)]
#[command(name = "chunky", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Propose byte-aligned meta-block ends in a served brotli stream.
    Flushes(flushes::Args),
    /// Reference-curve sizes for C and rust-brotli along call schedules.
    Baseline(baseline::Args),
    /// Markdown tables from baseline JSONL.
    Summarize(summarize::Args),
    /// Each quality's effective match reach, buffered and streamed (RE-002).
    Reach(reach::Args),
    /// Per-stream heap memory by quality and window (D-025).
    Mem(mem::Args),
    /// Parse composition: literals, copies by distance, static dictionary.
    ParseStats(parse_stats::Args),
    /// Repeatability funnel: cross-response reuse potential (not a size ceiling).
    Funnel(funnel::Args),
    /// Held-out-origin chunk reuse per content type (generic tier, D-026).
    GenericReuse(generic_reuse::Args),
    /// Meta-block structure of a br stream (independent RFC 7932 decoder).
    Inspect(inspect::Args),
    /// CPU timing under hostlock exclusive, pinned to one core (D-023).
    Bench(bench::Args),
    /// Print the synthetic-fixture identity digest (native side of the wasm check).
    IdentityDigest(identity::Args),
    /// Explain M2 stream-encoder sizes against the reference curve on the corpus.
    Replay(replay::Args),
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Flushes(a) => flushes::run(a),
        Cmd::Baseline(a) => baseline::run(a),
        Cmd::Summarize(a) => summarize::run(a),
        Cmd::Reach(a) => reach::run(a),
        Cmd::Mem(a) => mem::run(a),
        Cmd::ParseStats(a) => parse_stats::run(a),
        Cmd::Bench(a) => bench::run(a),
        Cmd::Funnel(a) => funnel::run(a),
        Cmd::GenericReuse(a) => generic_reuse::run(a),
        Cmd::Inspect(a) => inspect::run(a),
        Cmd::IdentityDigest(a) => identity::run(a),
        Cmd::Replay(a) => replay::run(a),
    }
}
