//! Builds the vendored C google/brotli sources (third_party/google-brotli,
//! D-023) with clang at -O2 -DNDEBUG and no -march, so baselines are
//! comparable across hosts. The exact compiler and flags are exported to the
//! crate as BROTLI_C_COMPILER and BROTLI_C_FLAGS for every report to record.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/google-brotli/c");
    let mut files = Vec::new();
    for dir in ["common", "dec", "enc"] {
        let mut v: Vec<PathBuf> = std::fs::read_dir(root.join(dir))
            .expect("vendored C brotli sources")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "c"))
            .collect();
        v.sort();
        files.extend(v);
    }
    let mut build = cc::Build::new();
    build
        .compiler("clang")
        .opt_level(2)
        .debug(false)
        .define("NDEBUG", None)
        .include(root.join("include"))
        .warnings(false)
        .files(&files);
    let tool = build.get_compiler();
    let flags: Vec<String> = tool
        .args()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    println!(
        "cargo:rustc-env=BROTLI_C_COMPILER={}",
        tool.path().display()
    );
    println!("cargo:rustc-env=BROTLI_C_FLAGS={}", flags.join(" "));
    println!("cargo:rerun-if-changed={}", root.display());
    build.compile("brotlic");
}
