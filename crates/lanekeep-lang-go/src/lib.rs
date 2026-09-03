//! Go language support for lanekeep.
//!
//! The tree-sitter Go grammar, plus the syntactic binding resolution in [`binding`].
//!
//! One grammar, one extension. Go has no dialect split to model — no TSX-style fork, no stub
//! files — so this is the simplest of the language crates in that respect and the most
//! involved in another: Go's scoping is genuinely block-structured, and unlike Python it is
//! also *order-independent* at package level, which the resolver has to model directly.
//!
//! Like the Python crate, this one does not touch `lanekeep-core`. It adds binding kinds to
//! `lanekeep-lang`, because Go names things the earlier languages have no word for — a
//! struct is not a `class`, a method receiver is not quite a `param` — and reusing the
//! nearest existing kind would answer `ctx.bindingKind` with something untrue.

pub mod binding;

use std::sync::Arc;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use crate::binding::GoBindingResolver;

/// Built once rather than per call: the resolver is stateless, and a host context holds it
/// for the life of a file.
static RESOLVER: std::sync::LazyLock<Arc<dyn BindingResolver>> =
    std::sync::LazyLock::new(|| Arc::new(GoBindingResolver));

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
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_GO_ANALYSIS_HASH"))
}

/// Go: `.go`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Go;

impl Language for Go {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("go")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Only `.go`. Generated files carry the same extension and are excluded by path,
        // not by language — a rule about generated code is a rule, not a grammar.
        &["go"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn analysis_identity(&self) -> [u8; 32] {
        analysis_identity()
    }
}

/// Register every language this crate provides.
///
/// # Errors
///
/// Propagates [`RegistryError`] if the registry already claims this identifier or this
/// extension.
pub fn register_all(registry: &mut LanguageRegistry) -> Result<(), RegistryError> {
    registry.register(Arc::new(Go))
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
            .set_language(&Go.grammar())
            .expect("the grammar loads");
        parser
            .parse(source, None)
            .is_some_and(|tree| !tree.root_node().has_error())
    }

    #[test]
    fn claims_the_go_extension() {
        assert_eq!(Go.extensions(), ["go"]);
        assert_eq!(Go.id().as_str(), "go");
    }

    #[test]
    fn a_registry_resolves_go_files_by_path() {
        let registry = registry();
        for path in ["main.go", "internal/app/server.go", "MAIN.GO"] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "go",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        assert!(parses_cleanly("package main\n"));
        assert!(parses_cleanly("package main\n\nimport \"fmt\"\n"));
        assert!(parses_cleanly(
            "package main\n\nimport (\n\tf \"fmt\"\n\t_ \"embed\"\n)\n"
        ));
        assert!(parses_cleanly(
            "package a\n\nfunc (r *Repo) Get(id string) (*User, error) { return nil, nil }\n"
        ));
        assert!(parses_cleanly(
            "package a\n\ntype Store interface{ Get(string) error }\n"
        ));
        assert!(parses_cleanly(
            "package a\n\nfunc F[T any](x T) T { return x }\n"
        ));
        assert!(parses_cleanly(
            "package a\n\nfunc f() {\n\tfor i, v := range xs {\n\t\t_ = i\n\t\t_ = v\n\t}\n}\n"
        ));
        assert!(parses_cleanly(
            "package a\n\nfunc f(v any) {\n\tswitch t := v.(type) {\n\tcase int:\n\t\t_ = t\n\t}\n}\n"
        ));
        assert!(parses_cleanly(
            "package a\n\nfunc f() {\n\tdefer func() { recover() }()\n\tgo work()\n}\n"
        ));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        // Written down, it stops tracking the thing it exists to track the first time
        // someone forgets to update it.
        assert_eq!(Go.grammar_abi(), Go.grammar().abi_version());
    }

    #[test]
    fn go_offers_a_resolver() {
        assert!(
            Go.resolver().is_some(),
            "a language with no resolver gives rules nothing to reason about"
        );
    }
}
