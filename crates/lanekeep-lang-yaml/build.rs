//! Fold this crate's own sources into the digest `analysis_identity` returns.
//!
//! The same walk every language crate's `build.rs` does, for the reason
//! `crates/lanekeep-types/build.rs` gives: a hand-maintained list of files would let one added
//! but not listed be a silent gap in a cache key.
//!
//! This crate has no resolver, so what the digest covers is smaller than elsewhere — but not
//! nothing. `src/lib.rs` decides which extensions are YAML, which is which files a `yaml` rule
//! runs on, and that belongs in the key as much as a resolver's scope list does.

#![expect(
    clippy::expect_used,
    reason = "this crate's own committed src/ is the only input; a failure here means a \
              broken checkout that must stop the build loudly"
)]

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");

    let src =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this")).join("src");

    let mut files = Vec::new();
    collect(&src, &mut files);
    // Sorted, so the digest does not depend on the order the filesystem happened to hand
    // entries back in.
    files.sort();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-lang-yaml-analysis-v1");
    for file in &files {
        let relative = file.strip_prefix(&src).unwrap_or(file);
        let path = relative.to_string_lossy().replace('\\', "/");
        let body = std::fs::read(file).expect("a file the walk just found is readable");
        // Length-prefixed, the same framing the ruleset hash uses and for the same reason:
        // without it, two different sets of files could fold to identical bytes.
        length_prefixed(&mut hasher, path.as_bytes());
        length_prefixed(&mut hasher, &body);
    }

    println!(
        "cargo:rustc-env=LANEKEEP_LANG_YAML_ANALYSIS_HASH={}",
        hasher.finalize().to_hex()
    );
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("the crate has a src directory");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn length_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
