//! chunky encoder core.
//!
//! The core has no filesystem, threads or clocks, and builds for
//! `wasm32-unknown-unknown`. Wrappers (CLI, Worker) supply buffers, explicit
//! logical flush events and artifact handles. `reference` drives the
//! rust-brotli reference encoder (the M1 curve) and captures its parse;
//! `stream` is the M2 per-flush encoder over the fork's external-command
//! writer, with the D-025 `Engine`/`Stream` shape.

pub mod fixtures;
pub mod inspect;
pub mod reference;
pub mod selftest;
pub mod stream;
