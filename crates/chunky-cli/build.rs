//! Record the compiling rustc's version for report provenance (D-023), so
//! reports do not depend on `rustc` being on PATH at run time.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let v = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=CHUNKY_RUSTC_VERSION={v}");
}
