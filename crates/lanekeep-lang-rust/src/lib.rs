//! Rust language support for lanekeep.
//!
//! The tree-sitter Rust grammar, plus the syntactic binding resolution in [`binding`].
//!
//! One grammar, one extension, and the most intricate resolver of the four — not because
//! Rust's scoping is unusual (it is ordinary block scoping, with order-independent items like
//! Go) but because Rust binds names through *patterns*. A single `let` can bind several names,
//! and the shape doing it also names constructors that are matched against rather than bound.
//! [`binding`] opens with what that costs if you get it wrong.
//!
//! Like the Python and Go crates, this one does not touch `lanekeep-core`. It adds two binding
//! kinds to `lanekeep-lang` — `module` and `trait` — because Rust names things the earlier
//! languages have no word for, and the nearest existing kind would be untrue: a module is not
//! a type, and a trait is not a struct.

pub mod binding;

use std::sync::Arc;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use crate::binding::RustBindingResolver;

/// Built once rather than per call: the resolver is stateless, and a host context holds it
/// for the life of a file.
static RESOLVER: std::sync::LazyLock<Arc<dyn BindingResolver>> =
    std::sync::LazyLock::new(|| Arc::new(RustBindingResolver));

/// Rust: `.rs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rust;

impl Language for Rust {
    fn resolver(&self) -> Option<Arc<dyn BindingResolver>> {
        Some(Arc::clone(&RESOLVER))
    }

    fn id(&self) -> LanguageId {
        LanguageId::new("rust")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Only `.rs`. Generated code carries the same extension and is excluded by path, not
        // by language — a rule about generated code is a rule, not a grammar.
        &["rs"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }
}

/// Register every language this crate provides.
///
/// # Errors
///
/// Propagates [`RegistryError`] if the registry already claims this identifier or this
/// extension.
pub fn register_all(registry: &mut LanguageRegistry) -> Result<(), RegistryError> {
    registry.register(Arc::new(Rust))
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
            .set_language(&Rust.grammar())
            .expect("the grammar loads");
        parser
            .parse(source, None)
            .is_some_and(|tree| !tree.root_node().has_error())
    }

    #[test]
    fn claims_the_rust_extension() {
        assert_eq!(Rust.extensions(), ["rs"]);
        assert_eq!(Rust.id().as_str(), "rust");
    }

    #[test]
    fn a_registry_resolves_rust_files_by_path() {
        let registry = registry();
        for path in ["main.rs", "src/lib.rs", "MAIN.RS"] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "rust",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        assert!(parses_cleanly("fn main() {}\n"));
        assert!(parses_cleanly("use std::io::{Read, Write as W};\n"));
        assert!(parses_cleanly("pub trait Store { fn get(&self) -> u8; }\n"));
        assert!(parses_cleanly(
            "impl<T: Clone> Repo<T> {\n    pub fn get(&self) -> Option<T> { None }\n}\n"
        ));
        assert!(parses_cleanly(
            "fn f(o: Option<u8>) {\n    if let Some(v) = o { let _ = v; }\n}\n"
        ));
        assert!(parses_cleanly(
            "fn f(r: R) {\n    match r { Ok(n) | Err(n) => { let _ = n; } }\n}\n"
        ));
        assert!(parses_cleanly("async fn f() -> Result<(), E> { Ok(()) }\n"));
        assert!(parses_cleanly(
            "macro_rules! m { ($x:expr) => { $x + 1 }; }\n"
        ));
        assert!(parses_cleanly("fn f<'a>(s: &'a str) -> &'a str { s }\n"));
        assert!(parses_cleanly(
            "#[derive(Debug)]\npub struct S { pub a: u8 }\n"
        ));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        // Written down, it stops tracking the thing it exists to track the first time
        // someone forgets to update it.
        assert_eq!(Rust.grammar_abi(), Rust.grammar().abi_version());
    }

    #[test]
    fn rust_offers_a_resolver() {
        assert!(
            Rust.resolver().is_some(),
            "a language with no resolver gives rules nothing to reason about"
        );
    }
}
