//! What every report records (D-023): C tag, compiler and flags, the Rust
//! pin, the fork base, the git revision and the host.

use serde_json::{Value, json};

pub const FORK_BASE: &str = "dropbox/rust-brotli 6518a4e (9.0.0), features std+portable-float";

fn cmd(c: &str, args: &[&str]) -> String {
    std::process::Command::new(c)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn collect() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|x| x.trim().to_string())
        })
        .unwrap_or_default();
    let dirty = !cmd("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty();
    json!({
        "c_brotli": format!("google/brotli {} ({})", brotli_c::C_BROTLI_TAG, &brotli_c::C_BROTLI_COMMIT[..12]),
        "c_compiler": cmd(brotli_c::C_COMPILER, &["--version"]).lines().next().unwrap_or("").to_string(),
        "c_flags": brotli_c::C_FLAGS,
        "rustc": env!("CHUNKY_RUSTC_VERSION"),
        "rust_brotli": FORK_BASE,
        "git": format!("{}{}", cmd("git", &["rev-parse", "--short", "HEAD"]), if dirty { "+dirty" } else { "" }),
        "cpu": cpu,
        "kernel": cmd("uname", &["-r"]),
    })
}

pub fn markdown(p: &Value) -> String {
    let g = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
    format!(
        "C: {} built by {} with `{}`. Rust: {}; {}. Repo {}. Host: {}, Linux {}.\n",
        g("c_brotli"),
        g("c_compiler"),
        g("c_flags"),
        g("rustc"),
        g("rust_brotli"),
        g("git"),
        g("cpu"),
        g("kernel")
    )
}
