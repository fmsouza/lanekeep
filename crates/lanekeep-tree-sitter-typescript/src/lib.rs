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
