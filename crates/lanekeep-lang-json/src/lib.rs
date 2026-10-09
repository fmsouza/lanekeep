//! JSON language support for lanekeep.
//!
//! The tree-sitter JSON grammar, and nothing else: JSON declares no names, so there is no
//! binding resolution to offer and [`Language::resolver`] keeps its `None` default. A rule
//! asking `ctx.bindingKind` about a JSON node gets nothing back, which is the honest answer
//! rather than a placeholder for one.
//!
//! The first of the data-format languages (#283). What it shares with the ones after it is
//! exactly that absence; what it does not share is comment syntax, which is why each is its own
//! crate rather than one crate for all of them.
//!
//! # Comments, and therefore suppressions
//!
//! JSON has no comments. `tree-sitter-json` accepts `//` and `/* */` anyway, as extras, which is
//! what makes a JSONC file — VS Code settings, a `tsconfig.json` — parse cleanly, and since
//! suppression directives are found by a text scan, a directive in such a comment works. In a
//! strict-JSON file a comment parses here and breaks every other consumer of the file, so there
//! a violation is acknowledged through the rule's own options or a path `exclude` instead.
//!
//! # Trailing commas fault
//!
//! The grammar separates members with commas and allows none after the last, so `{"a": 1,}` —
//! common in JSONC — parses with an `ERROR` node, and `lanekeep/parse` reports it whenever a
//! `json` rule runs on that file. A test pins that, so a grammar bump that changes it is noticed
//! rather than discovered.

use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use std::sync::Arc;

/// What this crate's analysis *is*, as a digest of every source file that decides an answer.
///
/// A cache key input, returned by [`Json::analysis_identity`]. Derived by `build.rs` from a walk
/// over `src/` rather than hand-maintained. There is no resolver here, but this file decides
/// which extensions are JSON — which files a `json` rule runs on — so it is not an empty term.
#[must_use]
pub fn analysis_identity() -> [u8; 32] {
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_JSON_ANALYSIS_HASH"))
}

/// JSON: `.json`, and `.jsonc` for the dialect with comments.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json;

impl Language for Json {
    fn id(&self) -> LanguageId {
        LanguageId::new("json")
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Not `json5`: unquoted keys and trailing commas make it a different grammar, and
        // claiming it would put every JSON5 file behind a `lanekeep/parse` report.
        &["json", "jsonc"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_json::LANGUAGE.into()
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
    registry.register(Arc::new(Json))
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
            .set_language(&Json.grammar())
            .expect("the grammar loads");
        parser.parse(source, None).expect("parses")
    }

    fn parses_cleanly(source: &str) -> bool {
        !parse(source).root_node().has_error()
    }

    #[test]
    fn claims_the_json_extensions() {
        assert_eq!(Json.extensions(), ["json", "jsonc"]);
        assert_eq!(Json.id().as_str(), "json");
    }

    #[test]
    fn a_registry_resolves_json_files_by_path() {
        let registry = registry();
        for path in [
            "package.json",
            "locales/en/messages.json",
            ".vscode/settings.jsonc",
            "CONFIG.JSON",
        ] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "json",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
        assert!(
            registry.for_path("data.json5").is_none(),
            "JSON5 is a different grammar"
        );
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        assert!(parses_cleanly("{}\n"));
        assert!(parses_cleanly("[]\n"));
        assert!(parses_cleanly(
            "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \
             \"n\": -1.5e3,\n  \"none\": null,\n  \"deps\": {\"a\": \"^1\"},\n  \
             \"files\": [\"a\", \"b\"]\n}\n"
        ));
        assert!(parses_cleanly(
            "{\"greeting\": \"h\\u00e9llo \\\"quoted\\\"\"}\n"
        ));
    }

    /// JSONC: comments are extras, so a directive in one is reachable by the text scan.
    #[test]
    fn parses_comments_as_extras() {
        assert!(parses_cleanly("// a line comment\n{\"a\": 1}\n"));
        assert!(parses_cleanly("{\n  /* a block */ \"a\": 1\n}\n"));
        let tree = parse("{\n  // note\n  \"a\": 1\n}\n");
        let mut cursor = tree.walk();
        let object = tree.root_node().named_child(0).expect("an object");
        assert!(
            object
                .named_children(&mut cursor)
                .any(|child| child.kind() == "comment"),
            "{}",
            tree.root_node().to_sexp()
        );
    }

    /// Pinned so a grammar bump that starts accepting them is noticed — the docs say a
    /// trailing comma is reported by `lanekeep/parse`.
    #[test]
    fn a_trailing_comma_faults() {
        assert!(!parses_cleanly("{\"a\": 1,}\n"));
        assert!(!parses_cleanly("[1, 2,]\n"));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        assert_eq!(Json.grammar_abi(), Json.grammar().abi_version());
    }

    #[test]
    fn json_has_no_resolver() {
        assert!(
            Json.resolver().is_none(),
            "JSON declares no names; a resolver here would answer about nothing"
        );
        assert!(Json.flow_analyzer().is_none());
        assert!(Json.obligation_analyzer().is_none());
    }

    #[test]
    fn the_analysis_identity_is_derived() {
        assert_ne!(Json.analysis_identity(), [0; 32]);
    }
}
