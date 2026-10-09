//! tree-sitter-typescript 0.23.2's TypeScript and TSX grammars, regenerated with two fixes.
//!
//! Upstream has had no grammar change since 0.23.2 (2024-11), and two valid forms come back
//! from it as `ERROR` nodes — which lanekeep reports as `lanekeep/parse`, and inside which no
//! rule's query matches:
//!
//! - `export type * from './m'` and `export type * as ns from './m'` (TypeScript 5.0;
//!   tree-sitter/tree-sitter-typescript#348).
//! - `f<typeof import('m')>()`, Vitest's `importOriginal<typeof import('./m')>()` idiom
//!   (tree-sitter/tree-sitter-typescript#367). With an argument it was worse:
//!   `f<typeof import('m')>(1)` parsed *clean*, as two comparisons.
//!
//! `grammar/patch.py` says what changed and why, and `grammar/regenerate.sh` — run through
//! `just typescript-grammar` — rebuilds everything under `typescript/src` and `tsx/src` from
//! upstream's published crate, after first proving its tree-sitter CLI reproduces upstream's
//! own `parser.c` byte for byte. Nothing under those two directories is edited by hand.
//!
//! The grammars are renamed `lanekeep_typescript` and `lanekeep_tsx`, so the C symbols cannot
//! collide with upstream's in a binary that links both crates. That is the only reason: the name
//! is invisible through `tree_sitter::Language`, whose `name()` is `None` below ABI 15.
//!
//! When upstream releases a grammar that fixes both, this crate is deleted and the workspace
//! points back at `tree-sitter-typescript`.

use tree_sitter_language::LanguageFn;

#[expect(
    unsafe_code,
    reason = "the two entry points the generated parsers export; declaring a C function is \
              unsafe by construction"
)]
unsafe extern "C" {
    fn tree_sitter_lanekeep_typescript() -> *const ();
    fn tree_sitter_lanekeep_tsx() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for TypeScript: `.ts`, `.mts`, `.cts`.
#[expect(
    unsafe_code,
    reason = "`from_raw` trusts that the pointer is a generated parser's language function, \
              which `build.rs` compiles from this crate's own `typescript/src/parser.c`"
)]
pub const LANGUAGE_TYPESCRIPT: LanguageFn =
    unsafe { LanguageFn::from_raw(tree_sitter_lanekeep_typescript) };

/// The tree-sitter [`LanguageFn`] for TSX: `.tsx`.
#[expect(
    unsafe_code,
    reason = "`from_raw` trusts that the pointer is a generated parser's language function, \
              which `build.rs` compiles from this crate's own `tsx/src/parser.c`"
)]
pub const LANGUAGE_TSX: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_lanekeep_tsx) };

/// The TypeScript grammar's `node-types.json`.
pub const TYPESCRIPT_NODE_TYPES: &str = include_str!("../typescript/src/node-types.json");

/// The TSX grammar's `node-types.json`.
pub const TSX_NODE_TYPES: &str = include_str!("../tsx/src/node-types.json");

/// A blake3 digest, as lowercase hex, of every file the two parsers are compiled from.
///
/// A cache key input: `lanekeep-lang-js` folds it into its `analysis_identity`, because the
/// grammar term of the key reads only a grammar's shape and a regeneration can change the parse
/// tables without changing the shape.
pub const SOURCE_DIGEST: &str = env!("LANEKEEP_TREE_SITTER_TYPESCRIPT_SOURCE_HASH");

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(language: LanguageFn, source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.into())
            .expect("grammar loads");
        parser.parse(source, None).expect("parser returns a tree")
    }

    /// Every file `build.rs` compiles or includes, in its order and framing. Restated rather
    /// than shared, so a file added to the build and not to the digest fails here.
    #[test]
    fn the_source_digest_covers_every_compiled_input() {
        let inputs: [(&str, &[u8]); 11] = [
            ("common/scanner.h", include_bytes!("../common/scanner.h")),
            ("tsx/src/parser.c", include_bytes!("../tsx/src/parser.c")),
            ("tsx/src/scanner.c", include_bytes!("../tsx/src/scanner.c")),
            (
                "tsx/src/tree_sitter/alloc.h",
                include_bytes!("../tsx/src/tree_sitter/alloc.h"),
            ),
            (
                "tsx/src/tree_sitter/array.h",
                include_bytes!("../tsx/src/tree_sitter/array.h"),
            ),
            (
                "tsx/src/tree_sitter/parser.h",
                include_bytes!("../tsx/src/tree_sitter/parser.h"),
            ),
            (
                "typescript/src/parser.c",
                include_bytes!("../typescript/src/parser.c"),
            ),
            (
                "typescript/src/scanner.c",
                include_bytes!("../typescript/src/scanner.c"),
            ),
            (
                "typescript/src/tree_sitter/alloc.h",
                include_bytes!("../typescript/src/tree_sitter/alloc.h"),
            ),
            (
                "typescript/src/tree_sitter/array.h",
                include_bytes!("../typescript/src/tree_sitter/array.h"),
            ),
            (
                "typescript/src/tree_sitter/parser.h",
                include_bytes!("../typescript/src/tree_sitter/parser.h"),
            ),
        ];
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lanekeep-tree-sitter-typescript-sources-v1");
        for (path, bytes) in inputs {
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        assert_eq!(SOURCE_DIGEST, hasher.finalize().to_hex().as_str());
    }

    /// `grammar/digests.txt` is what `regenerate.sh` last wrote. A file edited by hand, or a
    /// `patch.py` edited without regenerating, disagrees with it — and since the two `parser.c`
    /// files are 8 MB each and left out of diffs, this is where such an edit shows.
    #[test]
    fn the_committed_grammar_is_the_last_regeneration() {
        use sha2::Digest as _;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("grammar/digests.txt"))
            .expect("grammar/digests.txt exists");
        let mut listed = Vec::new();
        for line in manifest.lines().filter(|line| !line.starts_with('#')) {
            let (digest, path) = line.split_once("  ").expect("`<sha256>  <path>`");
            let bytes = std::fs::read(root.join(path)).expect("a listed file exists");
            let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
            assert_eq!(
                actual, digest,
                "{path} is not what `just typescript-grammar` last wrote — regenerate it rather \
                 than editing it"
            );
            listed.push(path.to_owned());
        }
        let mut expected = vec!["common/scanner.h".to_owned(), "grammar/patch.py".to_owned()];
        for language in ["tsx", "typescript"] {
            for file in [
                "grammar.json",
                "node-types.json",
                "parser.c",
                "scanner.c",
                "tree_sitter/alloc.h",
                "tree_sitter/array.h",
                "tree_sitter/parser.h",
            ] {
                expected.push(format!("{language}/src/{file}"));
            }
        }
        expected.sort();
        assert_eq!(listed, expected, "the manifest covers every generated file");
    }

    #[test]
    fn both_grammars_load_at_the_abi_upstream_shipped() {
        // ABI 14, as upstream's 0.23.2 was generated. A regeneration at 15 would change what
        // `Language::metadata()` and `name()` answer, which `lanekeep-lang`'s grammar digest
        // documents relying on.
        for language in [LANGUAGE_TYPESCRIPT, LANGUAGE_TSX] {
            let language: tree_sitter::Language = language.into();
            assert_eq!(language.abi_version(), 14);
        }
    }

    #[test]
    fn the_two_grammars_are_the_two_upstream_ships() {
        // Angle-bracket assertion: TypeScript only. JSX: TSX only.
        assert!(
            !tree(LANGUAGE_TYPESCRIPT, "const a = <T>b;")
                .root_node()
                .has_error()
        );
        assert!(
            tree(LANGUAGE_TSX, "const a = <T>b;")
                .root_node()
                .has_error()
        );
        assert!(
            !tree(LANGUAGE_TSX, "const a = <div/>;")
                .root_node()
                .has_error()
        );
    }
}
