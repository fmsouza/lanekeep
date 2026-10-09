//! Compile the two vendored parsers and their external scanners, as upstream's own build script
//! does, and digest every byte that goes into them.
//!
//! The digest is `SOURCE_DIGEST`, which `lanekeep-lang-js` folds into its `analysis_identity`.
//! The grammar term of the cache key reads a grammar's *shape* — node kinds, fields, counts —
//! and a regeneration that changes only the parse tables moves none of that, so without this a
//! warm cache would replay trees the shipped grammar no longer builds.

#![expect(
    clippy::expect_used,
    reason = "this crate's own committed sources are the only input; a failure here means a \
              broken checkout that must stop the build loudly"
)]

use std::path::Path;

/// Every file compiled or included, sorted. `src/lib.rs`'s
/// `the_source_digest_covers_every_compiled_input` restates the list.
const INPUTS: [&str; 11] = [
    "common/scanner.h",
    "tsx/src/parser.c",
    "tsx/src/scanner.c",
    "tsx/src/tree_sitter/alloc.h",
    "tsx/src/tree_sitter/array.h",
    "tsx/src/tree_sitter/parser.h",
    "typescript/src/parser.c",
    "typescript/src/scanner.c",
    "typescript/src/tree_sitter/alloc.h",
    "typescript/src/tree_sitter/array.h",
    "typescript/src/tree_sitter/parser.h",
];

fn main() {
    let mut build = cc::Build::new();
    build
        .include("typescript/src")
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wno-unused-parameter");
    for language in ["typescript", "tsx"] {
        let src = Path::new(language).join("src");
        for file in ["parser.c", "scanner.c"] {
            build.file(src.join(file));
        }
    }

    // Length-prefixed, path and body alike, so two different file sets cannot fold to the
    // same bytes.
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-tree-sitter-typescript-sources-v1");
    for path in INPUTS {
        println!("cargo:rerun-if-changed={path}");
        let bytes = std::fs::read(path).expect("a committed source file is readable");
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    println!(
        "cargo:rustc-env=LANEKEEP_TREE_SITTER_TYPESCRIPT_SOURCE_HASH={}",
        hasher.finalize().to_hex()
    );

    build.compile("lanekeep-tree-sitter-typescript");
}
