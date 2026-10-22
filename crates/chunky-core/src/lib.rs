//! chunky encoder core.
//!
//! The core has no filesystem, threads or clocks, and builds for
//! `wasm32-unknown-unknown`. Wrappers (CLI, Worker) supply buffers, explicit
//! logical flush events and artifact handles. M1 holds only the reference
//! encoder driver; the `Engine`/`Stream` API (D-025) lands in M2.

pub mod reference;
