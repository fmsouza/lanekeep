//! TypeScript and JavaScript language support for lanekeep.
//!
//! tree-sitter grammars for TypeScript, TSX, JavaScript and JSX.
//!
//! TypeScript and TSX are separate languages rather than one language with two file
//! extensions, because they are genuinely different grammars: TSX gives up the
//! angle-bracket type assertion `<T>expr` so the same syntax can open a JSX element.
//! Parsing a `.tsx` file with the TypeScript grammar produces errors on valid code.

pub mod binding;
pub mod cfg;
mod cfg_build;

pub use cfg::{Block, BlockId, Cfg, Edge, EdgeKind};

use std::sync::Arc;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use crate::binding::JsBindingResolver;

/// The resolver every language in this crate shares.
///
/// Built once rather than per call: the resolver is stateless, and a host context needs to
/// hold it for the life of a file.
static RESOLVER: std::sync::LazyLock<Arc<dyn BindingResolver>> =
    std::sync::LazyLock::new(|| Arc::new(JsBindingResolver));

/// What this crate's analysis *is*, as a digest of every source file that decides an answer.
///
/// A cache key input, returned by every [`Language`] this crate registers. Derived by
/// `build.rs` from a walk over `src/` rather than hand-maintained: the alternative is a list
/// of files somebody has to remember to extend, and nothing detects a missed entry.
///
/// Shared by every language this crate registers, which is correct — they share one resolver,
/// so a change to it changes what all of them answer.
#[must_use]
pub fn analysis_identity() -> [u8; 32] {
    // Written by `build.rs`, which walks `src/` so that a file added but not listed cannot be
    // a silent gap.
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_JS_ANALYSIS_HASH"))
}

/// TypeScript without JSX: `.ts`, `.mts`, `.cts`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TypeScript;

/// TypeScript with JSX: `.tsx`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tsx;

/// JavaScript, including JSX: `.js`, `.mjs`, `.cjs`, `.jsx`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JavaScript;

impl Language for TypeScript {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("typescript")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // `.d.ts` needs no separate entry: its extension is `ts`.
        &["ts", "mts", "cts"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

impl Language for Tsx {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("tsx")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tsx"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

impl Language for JavaScript {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("javascript")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // The JavaScript grammar handles JSX, so `.jsx` needs no separate language.
        &["js", "mjs", "cjs", "jsx"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

/// Register every language this crate provides.
///
/// # Errors
///
/// Propagates [`RegistryError`] if the registry already claims one of these identifiers or
/// extensions.
pub fn register_all(registry: &mut LanguageRegistry) -> Result<(), RegistryError> {
    registry.register(Arc::new(TypeScript))?;
    registry.register(Arc::new(Tsx))?;
    registry.register(Arc::new(JavaScript))?;
    Ok(())
}

/// A registry containing exactly this crate's languages.
///
/// Use [`register_all`] instead when adding these to a registry that already holds others,
/// so a genuine conflict surfaces as an error rather than a panic.
///
/// # Panics
///
/// Only if the three languages defined above were to claim the same identifier or
/// extension as each other, which is decided entirely by this file's contents and asserted
/// by `every_language_registers_without_conflict`.
#[expect(
    clippy::expect_used,
    reason = "the registry starts empty and is filled only from this file, so the failure \
              cases are a duplicate id or extension among three constants — a test asserts \
              they do not collide. Returning Result here would push an unreachable error \
              path onto every caller."
)]
#[must_use]
pub fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    register_all(&mut registry).expect("built-in languages do not conflict");
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(language: &dyn Language, source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.grammar())
            .expect("grammar loads");
        parser.parse(source, None).expect("parser returns a tree")
    }

    fn parses_cleanly(language: &dyn Language, source: &str) -> bool {
        !parse(language, source).root_node().has_error()
    }

    #[test]
    fn every_language_registers_without_conflict() {
        // `registry()` panics on conflict, so this asserts the panic path is unreachable
        // rather than trusting the comment that says so.
        let registry = registry();
        assert_eq!(registry.len(), 3);

        let ids: Vec<&str> = registry.languages().map(|l| l.id().as_str()).collect();
        assert_eq!(ids, ["javascript", "tsx", "typescript"]);
    }

    #[test]
    fn extensions_map_to_the_right_language() {
        let registry = registry();
        let cases = [
            ("src/a.ts", "typescript"),
            ("src/a.mts", "typescript"),
            ("src/a.cts", "typescript"),
            ("src/types.d.ts", "typescript"),
            ("src/Button.tsx", "tsx"),
            ("src/a.js", "javascript"),
            ("src/a.mjs", "javascript"),
            ("src/a.cjs", "javascript"),
            ("src/Button.jsx", "javascript"),
        ];

        for (path, expected) in cases {
            let language = registry
                .for_path(path)
                .unwrap_or_else(|| panic!("no language for {path}"));
            assert_eq!(
                language.id().as_str(),
                expected,
                "wrong language for {path}"
            );
        }
    }

    #[test]
    fn unrelated_extensions_are_not_claimed() {
        let registry = registry();
        for path in [
            "a.rs", "a.py", "a.json", "a.md", "a.mdx", "a.vue", "a.svelte", "README",
        ] {
            assert!(
                registry.for_path(path).is_none(),
                "should not have claimed {path}"
            );
        }
    }

    #[test]
    fn typescript_parses_type_syntax() {
        let ts = TypeScript;
        assert!(parses_cleanly(&ts, "const x: number = 1;"));
        assert!(parses_cleanly(&ts, "interface A { b: string }"));
        assert!(parses_cleanly(&ts, "export type B<T> = T | null;"));
        assert!(parses_cleanly(&ts, "enum E { A, B }"));
        assert!(parses_cleanly(
            &ts,
            "declare module 'x' { export const y: number }"
        ));
    }

    #[test]
    fn tsx_parses_jsx() {
        let tsx = Tsx;
        assert!(parses_cleanly(
            &tsx,
            "const a = <div className=\"x\">hi</div>;"
        ));
        assert!(parses_cleanly(&tsx, "const a = <><b/></>;"));
        // TSX is still TypeScript.
        assert!(parses_cleanly(&tsx, "const x: number = 1;"));
        assert!(parses_cleanly(&tsx, "function f<T,>(x: T): T { return x }"));
    }

    #[test]
    fn the_two_typescript_grammars_are_genuinely_different() {
        // The reason TSX is a separate language rather than another extension on
        // TypeScript. If this ever stops holding, the split has become dead weight.
        let ts = TypeScript;
        let tsx = Tsx;

        // Angle-bracket assertion: valid TypeScript, ambiguous with JSX so absent from TSX.
        let assertion = "const a = <string>value;";
        assert!(
            parses_cleanly(&ts, assertion),
            "TypeScript should accept a type assertion"
        );
        assert!(!parses_cleanly(&tsx, assertion), "TSX should not");

        // JSX: valid TSX, not TypeScript.
        let element = "const a = <div>hi</div>;";
        assert!(parses_cleanly(&tsx, element), "TSX should accept JSX");
        assert!(!parses_cleanly(&ts, element), "TypeScript should not");
    }

    #[test]
    fn javascript_parses_jsx_and_modern_syntax() {
        let js = JavaScript;
        assert!(parses_cleanly(&js, "const a = <div>hi</div>;"));
        assert!(parses_cleanly(
            &js,
            "export default async () => { await x?.y ?? z }"
        ));
        assert!(parses_cleanly(
            &js,
            "class A { #priv = 1; static { init() } }"
        ));
    }

    #[test]
    fn grammar_abi_is_reported_per_language() {
        // The ABI feeds the cache key. It has to be a real number read from the grammar,
        // and it has to be per-language: these grammars do not currently agree, so one
        // global constant would be wrong for at least one of them.
        let ts = TypeScript.grammar_abi();
        let tsx = Tsx.grammar_abi();
        let js = JavaScript.grammar_abi();

        for abi in [ts, tsx, js] {
            assert!(abi >= 13, "implausible ABI version {abi}");
        }
        assert_eq!(ts, tsx, "the two TypeScript grammars ship together");
        assert_ne!(
            ts, js,
            "these grammars currently differ; if they have converged, the per-language \
             ABI is no longer demonstrated by this test and needs another guard"
        );
    }

    #[test]
    fn parsing_invalid_source_yields_errors_rather_than_failing() {
        // tree-sitter always returns a tree. Parse failure surfaces as ERROR nodes, which
        // is what the parse-error diagnostic keys off — it is not an absent tree.
        let tree = parse(&TypeScript, "const x: = ;;; function {");
        assert!(tree.root_node().has_error());
    }

    #[test]
    fn parsing_empty_source_is_not_an_error() {
        let tree = parse(&TypeScript, "");
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().kind(), "program");
    }
}
