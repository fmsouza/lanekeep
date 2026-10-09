//! TypeScript and JavaScript language support for lanekeep.
//!
//! tree-sitter grammars for TypeScript, TSX, JavaScript and JSX.
//!
//! TypeScript and TSX are separate languages rather than one language with two file
//! extensions, because they are genuinely different grammars: TSX gives up the
//! angle-bracket type assertion `<T>expr` so the same syntax can open a JSX element.
//! Parsing a `.tsx` file with the TypeScript grammar produces errors on valid code.
//!
//! # The control-flow graph
//!
//! [`mod@cfg`] holds a per-function control-flow graph for TypeScript, TSX and JavaScript:
//! basic blocks split at every branch — short-circuit operators included, since `&&`, `??`
//! and `?.` are control flow inside an expression — and a `finally` emitted once per distinct
//! continuation rather than special-cased. Nothing in the engine calls it directly. #193's
//! obligation analysis and #194's taint analysis are its two consumers; obligation analysis
//! consumes the all-paths and reachability queries (`on_all_paths_from_any`,
//! `on_all_paths_within`, `reaches`, `reaches_avoiding`), which is why the graph and these
//! queries are public from a module neither of them exists in yet.
//!
//! It is not a [`Language`] method, unlike everything else this crate exposes that way.
//! Construction dispatches on tree-sitter node kind, never on a language identifier, so a
//! trait method would commit to a signature before a second implementor exists to check it
//! against — the opposite of the day-one stance `docs/architecture.md`'s `Language` trait
//! section gives [`Language`] itself: implement it before a second language exists, because
//! it is cheap now and impossible to retrofit.
//!
//! It lives in this crate rather than a `lanekeep-cfg` one on the precedent
//! `crates/lanekeep-nodes/src/lib.rs` sets: that crate lived in `lanekeep-js` until
//! `lanekeep-wasm` became a second engine needing its exact type, and was extracted then
//! rather than in anticipation. **What would move this out is a second language needing a
//! CFG** — only `cfg_build`'s construction reads node kinds; the language-agnostic part
//! today is just the block/edge model and the two traversals in [`mod@cfg`], and one
//! implementor is not enough to know where that seam actually belongs.

pub mod binding;
pub mod cfg;
mod cfg_build;
mod cfg_query;
mod flow;
mod obligation;

pub use cfg::{Block, BlockId, Cfg, Edge, EdgeKind};

use std::sync::Arc;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::flow::FlowAnalyzer;
use lanekeep_lang::obligation::ObligationAnalyzer;
use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use crate::binding::JsBindingResolver;
use crate::flow::JsFlowAnalyzer;

/// The resolver every language in this crate shares.
///
/// Built once rather than per call: the resolver is stateless, and a host context needs to
/// hold it for the life of a file.
static RESOLVER: std::sync::LazyLock<Arc<dyn BindingResolver>> =
    std::sync::LazyLock::new(|| Arc::new(JsBindingResolver));

/// The flow analyzer every language in this crate shares.
///
/// Built once for the same reason as [`RESOLVER`]: the analyzer is stateless and a host
/// context holds it for the life of a file.
static FLOW_ANALYZER: std::sync::LazyLock<Arc<dyn FlowAnalyzer>> =
    std::sync::LazyLock::new(|| Arc::new(JsFlowAnalyzer));

/// The obligation analyzer every language in this crate shares.
///
/// Built once rather than per call, same reasoning as [`RESOLVER`]: the analyzer is
/// stateless, and a host context needs to hold it for the life of a file.
static OBLIGATION: std::sync::LazyLock<Arc<dyn ObligationAnalyzer>> =
    std::sync::LazyLock::new(|| Arc::new(obligation::JsObligationAnalyzer));

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

    fn flow_analyzer(&self) -> Option<Arc<dyn FlowAnalyzer>> {
        Some(Arc::clone(&FLOW_ANALYZER))
    }

    fn obligation_analyzer(&self) -> Option<Arc<dyn ObligationAnalyzer>> {
        Some(Arc::clone(&OBLIGATION))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("typescript")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // `.d.ts` needs no separate entry: its extension is `ts`.
        &["ts", "mts", "cts"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        lanekeep_tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

impl Language for Tsx {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn flow_analyzer(&self) -> Option<Arc<dyn FlowAnalyzer>> {
        Some(Arc::clone(&FLOW_ANALYZER))
    }

    fn obligation_analyzer(&self) -> Option<Arc<dyn ObligationAnalyzer>> {
        Some(Arc::clone(&OBLIGATION))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("tsx")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tsx"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        lanekeep_tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

impl Language for JavaScript {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn flow_analyzer(&self) -> Option<Arc<dyn FlowAnalyzer>> {
        Some(Arc::clone(&FLOW_ANALYZER))
    }

    fn obligation_analyzer(&self) -> Option<Arc<dyn ObligationAnalyzer>> {
        Some(Arc::clone(&OBLIGATION))
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

    /// Both TypeScript grammars, for the forms that must read the same in either.
    fn both() -> [(&'static str, &'static dyn Language); 2] {
        [("typescript", &TypeScript), ("tsx", &Tsx)]
    }

    /// The tree, asserted clean, as an S-expression.
    fn clean_sexp(name: &str, language: &dyn Language, source: &str) -> String {
        let tree = parse(language, source);
        let sexp = tree.root_node().to_sexp();
        assert!(
            !tree.root_node().has_error(),
            "{name}: `{source}` should parse clean, got {sexp}"
        );
        sexp
    }

    /// TypeScript 5.0's type-only star re-exports, which upstream tree-sitter-typescript 0.23.2
    /// reads as an `ERROR` at the `*` (tree-sitter/tree-sitter-typescript#348; lanekeep#286).
    /// They read as the value forms do, `type` keyword and all.
    #[test]
    fn type_only_star_reexports_parse() {
        for (name, language) in both() {
            assert_eq!(
                clean_sexp(name, language, "export type * from './types';\n"),
                "(program (export_statement source: (string (string_fragment))))"
            );
            assert_eq!(
                clean_sexp(name, language, "export type * as ns from './types';\n"),
                "(program (export_statement (namespace_export (identifier)) source: (string \
                 (string_fragment))))"
            );
        }
    }

    /// `typeof import(...)` as the first type argument of a call: Vitest's
    /// `importOriginal<typeof import('./m')>()` idiom. Upstream 0.23.2 read it as
    /// `f < typeof import('x')` followed by a stray `()` (tree-sitter/tree-sitter-typescript#367;
    /// lanekeep#271, #286) because a static precedence discarded the type-argument reading before
    /// the `>` that decides it.
    #[test]
    fn a_typeof_import_type_argument_is_a_call() {
        let call = "(call_expression function: (identifier) type_arguments: (type_arguments \
                    (type_query (call_expression function: (import) arguments: (arguments \
                    (string (string_fragment)))))) arguments: (arguments))";
        for (name, language) in both() {
            assert_eq!(
                clean_sexp(name, language, "f<typeof import('x')>();\n"),
                format!("(program (expression_statement {call}))")
            );
            let idiom = clean_sexp(
                name,
                language,
                "async () => {\n  const actual = await importOriginal<typeof import('./dep')>();\n};\n",
            );
            assert!(
                idiom.contains(
                    "(call_expression function: (await_expression (identifier)) \
                                type_arguments: (type_arguments (type_query"
                ),
                "{name}: {idiom}"
            );
        }
    }

    /// With an argument the upstream misreading was silent: `f<typeof import('m')>(1)` parsed
    /// clean as `(f < typeof import('m')) > (1)`, so no `lanekeep/parse` could flag it. It is the
    /// call TypeScript itself reads.
    #[test]
    fn a_typeof_import_type_argument_with_an_argument_is_not_two_comparisons() {
        for (name, language) in both() {
            let sexp = clean_sexp(name, language, "f<typeof import('m')>(1);\n");
            assert!(
                sexp.starts_with("(program (expression_statement (call_expression"),
                "{name}: {sexp}"
            );
            assert!(!sexp.contains("binary_expression"), "{name}: {sexp}");
        }
    }

    /// The fix keeps both readings alive until the parser can tell; where the source really is a
    /// comparison it must stay one.
    #[test]
    fn a_comparison_against_typeof_import_stays_a_comparison() {
        for (name, language) in both() {
            let sexp = clean_sexp(name, language, "x < typeof import('m') > y;\n");
            assert!(
                sexp.starts_with(
                    "(program (expression_statement (binary_expression left: (binary_expression"
                ),
                "{name}: {sexp}"
            );
        }
    }

    #[test]
    fn parsing_empty_source_is_not_an_error() {
        let tree = parse(&TypeScript, "");
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().kind(), "program");
    }
}
