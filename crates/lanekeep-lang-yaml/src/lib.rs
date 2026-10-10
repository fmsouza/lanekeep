//! YAML language support for lanekeep.
//!
//! The tree-sitter YAML grammar from tree-sitter-grammars, and nothing else. YAML declares no
//! names a lexical scope could resolve — an anchor is a reference within one document, not a
//! declaration — so there is no binding resolver and [`Language::resolver`] keeps its `None`
//! default, as JSON's does (#283).
//!
//! # Comments, and therefore suppressions
//!
//! A YAML comment is `#` to the end of the line, and the grammar keeps each one as a `comment`
//! node. Suppression directives are found by a text scan, so a directive in one works with no
//! YAML-specific code anywhere.
//!
//! # A stream, not a document
//!
//! The root is `stream`, and every `---`-separated document in the file is a `document` under
//! it, so a query anchored at `(document)` runs once per document. A GitHub workflow, a
//! Kubernetes manifest list and a single-document config file all parse to that one shape.

use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use std::sync::Arc;

/// What this crate's analysis *is*, as a digest of every source file that decides an answer.
///
/// A cache key input, returned by [`Yaml::analysis_identity`]. Derived by `build.rs` from a walk
/// over `src/` rather than hand-maintained. There is no resolver here, but this file decides
/// which extensions are YAML — which files a `yaml` rule runs on — so it is not an empty term.
#[must_use]
pub fn analysis_identity() -> [u8; 32] {
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_YAML_ANALYSIS_HASH"))
}

/// YAML: `.yaml`, and `.yml`, which is the spelling most CI configurations use.
#[derive(Debug, Clone, Copy, Default)]
pub struct Yaml;

impl Language for Yaml {
    fn id(&self) -> LanguageId {
        LanguageId::new("yaml")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_yaml::LANGUAGE.into()
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
    registry.register(Arc::new(Yaml))
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
            .set_language(&Yaml.grammar())
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
    fn claims_the_yaml_extensions() {
        assert_eq!(Yaml.extensions(), ["yaml", "yml"]);
        assert_eq!(Yaml.id().as_str(), "yaml");
    }

    #[test]
    fn a_registry_resolves_yaml_files_by_path() {
        let registry = registry();
        for path in [
            ".github/workflows/ci.yml",
            "deploy/values.yaml",
            "CONFIG.YML",
        ] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "yaml",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
        assert!(registry.for_path("notes.yamlx").is_none());
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        // A GitHub workflow: nested block mappings, a flow sequence, a sequence of mappings and
        // a literal block scalar.
        assert!(parses_cleanly(
            "name: CI\non:\n  push:\n    branches: [main]\njobs:\n  build:\n    \
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      \
             - run: |\n          cargo test\n          cargo build\n"
        ));
        // Anchors, aliases, quoted scalars and a second document.
        assert!(parses_cleanly(
            "defaults: &d {retries: 3}\nprod:\n  <<: *d\n  name: \"prod\"\n  tag: 'v1'\n\
             ---\nempty: ~\nflag: true\n"
        ));
    }

    #[test]
    fn every_document_in_a_stream_is_its_own_node() {
        let tree = parse("a: 1\n---\nb: 2\n");
        let root = tree.root_node();
        assert_eq!(root.kind(), "stream");
        let mut cursor = root.walk();
        let documents = root
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "document")
            .count();
        assert_eq!(documents, 2, "{}", root.to_sexp());
    }

    /// The text scan finds a directive in any `#` comment; this pins that the grammar treats
    /// one as a comment too, rather than as part of a scalar.
    #[test]
    fn a_hash_comment_is_a_comment_node() {
        let source = "# leading\nruns-on: ubuntu-latest # trailing\n";
        assert!(parses_cleanly(source));
        let mut found = Vec::new();
        kinds(parse(source).root_node(), &mut found);
        assert_eq!(
            found.iter().filter(|kind| *kind == "comment").count(),
            2,
            "{found:?}"
        );
    }

    /// Pinned so a grammar bump that starts accepting it is noticed: an unclosed flow
    /// sequence is a parse fault, which `lanekeep/parse` reports under a `yaml` rule.
    #[test]
    fn an_unclosed_flow_sequence_faults() {
        assert!(!parses_cleanly("a: [1, 2\nb: 3\n"));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        assert_eq!(Yaml.grammar_abi(), Yaml.grammar().abi_version());
    }

    #[test]
    fn yaml_has_no_resolver() {
        assert!(
            Yaml.resolver().is_none(),
            "YAML declares no names; a resolver here would answer about nothing"
        );
        assert!(Yaml.flow_analyzer().is_none());
        assert!(Yaml.obligation_analyzer().is_none());
    }

    #[test]
    fn the_analysis_identity_is_derived() {
        assert_ne!(Yaml.analysis_identity(), [0; 32]);
    }
}
