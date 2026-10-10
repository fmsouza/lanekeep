//! CSS language support for lanekeep.
//!
//! The tree-sitter CSS grammar, and nothing else. There is no binding resolver and
//! [`Language::resolver`] keeps its `None` default, as JSON's does (#283) — but for a different
//! reason. CSS does name things: custom properties, keyframes, layers. What it lacks is
//! *lexical* scope. Which `--accent` a `var(--accent)` reads is decided by the cascade, against
//! a document, at run time, and a resolver answering "where was this declared" from one file
//! would be answering a question CSS does not ask.
//!
//! # Comments, and therefore suppressions
//!
//! CSS has one comment form, `/* */`, so every suppression directive in a stylesheet sits
//! inside a block comment. The scanner in `lanekeep-core` ends a directive where its block
//! comment closes, which is what lets `expires:` be the last thing in one.
//!
//! The grammar also accepts `//` line comments, as `js_comment` nodes — a concession to
//! preprocessors. That is not CSS, a browser does not read it as a comment, and the docs name
//! `/* */` as the form to use.
//!
//! # Not Sass, not Less
//!
//! `.scss`, `.sass` and `.less` are different grammars: `$x: 1px;` is a parse fault here. They
//! are not claimed, so a project's preprocessor sources are never put behind a
//! `lanekeep/parse` report by a `css` rule.

use lanekeep_lang::{Language, LanguageId, LanguageRegistry, RegistryError};

use std::sync::Arc;

/// What this crate's analysis *is*, as a digest of every source file that decides an answer.
///
/// A cache key input, returned by [`Css::analysis_identity`]. Derived by `build.rs` from a walk
/// over `src/` rather than hand-maintained. There is no resolver here, but this file decides
/// which extensions are CSS — which files a `css` rule runs on — so it is not an empty term.
#[must_use]
pub fn analysis_identity() -> [u8; 32] {
    lanekeep_lang::decode_hex32(env!("LANEKEEP_LANG_CSS_ANALYSIS_HASH"))
}

/// CSS: `.css`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Css;

impl Language for Css {
    fn id(&self) -> LanguageId {
        LanguageId::new("css")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["css"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_css::LANGUAGE.into()
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
    registry.register(Arc::new(Css))
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
            .set_language(&Css.grammar())
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
    fn claims_the_css_extension() {
        assert_eq!(Css.extensions(), ["css"]);
        assert_eq!(Css.id().as_str(), "css");
    }

    #[test]
    fn a_registry_resolves_css_files_by_path() {
        let registry = registry();
        for path in ["src/a.css", "styles/theme.module.css", "LEGACY.CSS"] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                "css",
                "{path}"
            );
        }
        assert!(registry.for_path("src/app.ts").is_none());
        for preprocessor in ["a.scss", "a.sass", "a.less"] {
            assert!(
                registry.for_path(preprocessor).is_none(),
                "{preprocessor} is a different grammar"
            );
        }
    }

    #[test]
    fn parses_the_syntax_rules_will_meet() {
        // Rule sets, a media query, `!important`, custom properties and `var()`, `calc()`.
        assert!(parses_cleanly(
            ":root { --accent: #0af; }\n.a { color: var(--accent); }\n\
             @media (max-width: 600px) { .b { margin: 0 !important; } }\n\
             .c { width: calc(100% - 2rem); }\n"
        ));
        // Nesting, at-rules and keyframes.
        assert!(parses_cleanly(
            ".card { .title { font-weight: 600; } &:hover { opacity: 0.9; } }\n\
             @import url(\"base.css\");\n\
             @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }\n"
        ));
    }

    /// CSS's one comment form, kept by the grammar as a `comment` node — the form a
    /// suppression directive in a stylesheet is written in.
    #[test]
    fn a_block_comment_is_a_comment_node() {
        let source = "/* leading */\n.a { color: red; /* inline */ }\n";
        assert!(parses_cleanly(source));
        let mut found = Vec::new();
        kinds(parse(source).root_node(), &mut found);
        assert_eq!(
            found.iter().filter(|kind| *kind == "comment").count(),
            2,
            "{found:?}"
        );
    }

    /// Pinned so a grammar bump that starts accepting it is noticed: Sass syntax is a parse
    /// fault, which is why `.scss` is not claimed.
    #[test]
    fn sass_syntax_faults() {
        assert!(!parses_cleanly("$x: 1px;\n.a { width: $x; }\n"));
    }

    #[test]
    fn the_grammar_abi_is_read_from_the_grammar() {
        assert_eq!(Css.grammar_abi(), Css.grammar().abi_version());
    }

    #[test]
    fn css_has_no_resolver() {
        assert!(
            Css.resolver().is_none(),
            "CSS names are scoped by the cascade, not lexically; a resolver would answer a \
             question CSS does not ask"
        );
        assert!(Css.flow_analyzer().is_none());
        assert!(Css.obligation_analyzer().is_none());
    }

    #[test]
    fn the_analysis_identity_is_derived() {
        assert_ne!(Css.analysis_identity(), [0; 32]);
    }
}
