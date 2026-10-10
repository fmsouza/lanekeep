//! TOML language support for lanekeep.
//!
//! The tree-sitter TOML grammar from tree-sitter-grammars (`tree-sitter-toml-ng`), and nothing
//! else. TOML declares no names — a key is data, not a binding — so there is no binding
//! resolver and [`Language::resolver`] keeps its `None` default, as JSON's does (#283).
//!
//! # Comments, and therefore suppressions
//!
//! A TOML comment is `#` to the end of the line, and the grammar keeps each one as a `comment`
//! node. Suppression directives are found by a text scan, so a directive in one works with no
//! TOML-specific code anywhere.
//!
//! # Extension only
//!
//! The registry matches a file by extension, so `Cargo.toml` and `pyproject.toml` are TOML and
//! `Cargo.lock` and `Pipfile`, which are TOML by content, are not. Claiming a file by name is a
//! registry change rather than one this crate can make.

use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use std::sync::Arc;

/// What this crate's analysis *is*, as a digest of every source file that decides an answer.
///
/// A cache key input, returned by [`Toml::analysis_identity`]. Derived by `build.rs` from a walk
/// over `src/` rather than hand-maintained. There is no resolver here, but this file decides
/// which extensions are TOML — which files a `toml` rule runs on — so it is not an empty term.
#[must_use]
pub fn analysis_identity() -> [u8; 32] {
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_TOML_ANALYSIS_HASH"))
}

/// TOML: `.toml`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Toml;

impl Language for Toml {
    fn id(&self) -> LanguageId {
        LanguageId::new("toml")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_toml_ng::LANGUAGE.into()
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
    registry.register(Arc::new(Toml))
}

/// A registry holding only this crate's languages.
///
/// # Panics
///
/// If this crate's own languages conflict, which no input can cause — it would be a bug
/// here rather than a user error.
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

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&Toml.grammar())
            .expect("the grammar loads");
        parser.parse(source, None).expect("parses")
    }

    fn parses_cleanly(source: &str) -> bool {
        !parse(source).root_node().has_error()
    }

    fn kinds(node: tree_sitter::Node<'_>, out: &mut Vec<String>) {
        out.push(node.kind().to_owned());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            kinds(child, out);
        }
    }

    #[test]
    fn claims_the_toml_extension() {
        assert_eq!(Toml.extensions(), ["toml"]);
        assert_eq!(Toml.id().as_str(), "toml");
    }

    #[test]
    fn a_registry_resolves_toml_files_by_path() {
        let registry = registry();
        for path in [
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "pyproject.toml",
            "CONFIG.TOML",
        ] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "toml",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
        assert!(
            registry.for_path("Cargo.lock").is_none(),
            "matched by extension, not by name"
        );
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        // A Cargo manifest: tables, an inline table holding an array, an array of tables, a
        // dotted key, and a workspace-inherited value.
        assert!(parses_cleanly(
            "[package]\nname = \"x\"\nversion.workspace = true\n\n[dependencies]\n\
             serde = { version = \"1\", features = [\"derive\"] }\n\n[[bin]]\nname = \"b\"\n\
             path = 'src/main.rs'\n"
        ));
        // The scalar forms: dates, floats, multi-line strings, booleans.
        assert!(parses_cleanly(
            "when = 1979-05-27T07:32:00Z\nday = 1979-05-27\npi = 3.14\nbig = 1_000\n\
             on = false\ntext = '''\nraw\n'''\nbasic = \"\"\"\nmulti\n\"\"\"\n"
        ));
    }

    /// The text scan finds a directive in any `#` comment; this pins that the grammar treats
    /// one as a comment too, rather than as part of a value.
    #[test]
    fn a_hash_comment_is_a_comment_node() {
        let source = "# leading\nname = \"x\" # trailing\n";
        assert!(parses_cleanly(source));
        let mut found = Vec::new();
        kinds(parse(source).root_node(), &mut found);
        assert_eq!(
            found.iter().filter(|kind| *kind == "comment").count(),
            2,
            "{found:?}"
        );
    }

    /// Pinned so a grammar bump that starts accepting it is noticed: a key with no value is a
    /// parse fault, which `lanekeep/parse` reports under a `toml` rule.
    #[test]
    fn a_key_with_no_value_faults() {
        assert!(!parses_cleanly("a =\nb = 1\n"));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        assert_eq!(Toml.grammar_abi(), Toml.grammar().abi_version());
    }

    #[test]
    fn toml_has_no_resolver() {
        assert!(
            Toml.resolver().is_none(),
            "TOML declares no names; a resolver here would answer about nothing"
        );
        assert!(Toml.flow_analyzer().is_none());
        assert!(Toml.obligation_analyzer().is_none());
    }

    #[test]
    fn the_analysis_identity_is_derived() {
        assert_ne!(Toml.analysis_identity(), [0; 32]);
    }
}
