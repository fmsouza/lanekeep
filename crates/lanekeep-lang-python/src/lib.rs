//! Python language support for lanekeep.
//!
//! The tree-sitter Python grammar, plus the syntactic binding resolution in [`binding`].
//!
//! One grammar covers every file this crate claims. Unlike the TypeScript family, where TSX
//! gives up `<T>expr` so the same syntax can open a JSX element, Python has no dialect split
//! to model — `.pyi` stub files are the same language with only declarations in them.
//!
//! This crate exists to answer the question §16's M4 poses: whether the `Language` trait
//! actually abstracts a language, or only describes the one it was written against. It does
//! not touch `lanekeep-core`. It does add binding kinds to `lanekeep-lang`, because Python
//! binds names in ways JavaScript has no word for — an assignment, a loop target, a context
//! manager — and reusing `var` for all three would answer `ctx.bindingKind` with something
//! untrue.

pub mod binding;

use std::sync::Arc;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use crate::binding::PythonBindingResolver;

/// Built once rather than per call: the resolver is stateless, and a host context holds it
/// for the life of a file.
static RESOLVER: std::sync::LazyLock<Arc<dyn BindingResolver>> =
    std::sync::LazyLock::new(|| Arc::new(PythonBindingResolver));

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
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_PYTHON_ANALYSIS_HASH"))
}

/// Python: `.py`, `.pyi`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Python;

impl Language for Python {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("python")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // `.pyi` is the same grammar: a stub file is Python with the bodies removed, and a
        // rule about imports or signatures is exactly as applicable there.
        &["py", "pyi"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

/// Register every language this crate provides.
///
/// # Errors
///
/// Propagates [`RegistryError`] if the registry already claims this identifier or one of
/// these extensions.
pub fn register_all(registry: &mut LanguageRegistry) -> Result<(), RegistryError> {
    registry.register(Arc::new(Python))
}

/// A registry holding only this crate's languages.
///
/// # Panics
///
/// If this crate's own languages conflict, which no input can cause — it would be a bug
/// here rather than a user error, and returning a `Result` nobody can act on would only
/// move the `unwrap` to every call site.
#[must_use]
pub fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    #[expect(
        clippy::expect_used,
        reason = "documented above: a conflict between this crate's own languages is a bug \
                  here, not a condition a caller can handle"
    )]
    {
        register_all(&mut registry).expect("built-in languages do not conflict");
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parses_cleanly(source: &str) -> bool {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&Python.grammar())
            .expect("the grammar loads");
        parser
            .parse(source, None)
            .is_some_and(|tree| !tree.root_node().has_error())
    }

    #[test]
    fn claims_the_python_extensions() {
        assert_eq!(Python.extensions(), ["py", "pyi"]);
        assert_eq!(Python.id().as_str(), "python");
    }

    #[test]
    fn a_registry_resolves_python_files_by_path() {
        let registry = registry();
        for path in ["src/app.py", "src/stubs.pyi", "src/App.PY"] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "python",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        assert!(parses_cleanly("import os\n"));
        assert!(parses_cleanly("from a.b import c as d\n"));
        assert!(parses_cleanly(
            "async def f(x: int = 1) -> str:\n    return str(x)\n"
        ));
        assert!(parses_cleanly("xs = [y for y in range(3) if y]\n"));
        assert!(parses_cleanly("match x:\n    case 1:\n        pass\n"));
        assert!(parses_cleanly(
            "with open(p) as f, open(q) as g:\n    pass\n"
        ));
        assert!(parses_cleanly("class C:\n    def m(self) -> None: ...\n"));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        // Written down, it stops tracking the thing it exists to track the first time
        // someone forgets to update it.
        assert_eq!(Python.grammar_abi(), Python.grammar().abi_version());
    }

    #[test]
    fn python_offers_a_resolver() {
        assert!(
            Python.resolver().is_some(),
            "a language with no resolver gives rules nothing to reason about"
        );
    }
}
