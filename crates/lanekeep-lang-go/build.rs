//! Fold this crate's own sources into the digest `analysis_identity` returns.
//!
//! A directory walk rather than a list of `include_str!` calls, for the reason
//! `crates/lanekeep-types/build.rs` gives: a list is hand-maintained, and a file added but
//! not listed would be a silent gap in a cache key — the exact failure the digest exists to
//! remove.
//!
//! Over-invalidating on purpose. Editing a comment in this crate discards every cached result
//! that used its resolver, which costs a recompute; the opposite error serves an answer
//! computed by code that no longer exists and gives no sign that it did.

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
    hasher.update(b"lanekeep-lang-go-analysis-v1");
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
        "cargo:rustc-env=LANEKEEP_LANG_GO_ANALYSIS_HASH={}",
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
