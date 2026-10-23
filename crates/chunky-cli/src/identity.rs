//! `chunky identity-digest`: prints `chunky_core::selftest::identity_digest()`
//! as 16 lowercase hex digits. `worker/test/identity.test.mjs` compares it
//! with the same digest computed in wasm under V8.

use anyhow::Result;

#[derive(clap::Args)]
pub struct Args {}

pub fn run(_a: Args) -> Result<()> {
    println!("{:016x}", chunky_core::selftest::identity_digest());
    Ok(())
}
