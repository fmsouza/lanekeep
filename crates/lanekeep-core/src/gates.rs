//! Rejecting files before they are read or parsed.
//!
//! Architecture §7.1 calls this the single largest performance lever available. A rule
//! scoped to `makeStyles` skips parsing every file whose bytes do not contain that string,
//! and parsing is the dominant cost.
//!
//! Gates are **purely an optimization**. Removing one changes which files get parsed, never
//! which violations get reported — a gate that rejects a file the handler would have
//! reported on is a bug in the rule, not a feature of the gate. The tests hold that line by
//! checking gates only ever narrow.
//!
//! They are evaluated in cost order:
//!
//! 1. **Path** — no file read at all.
//! 2. **Content** — one read, a substring scan, no parse.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use thiserror::Error;

use crate::location::FilePath;

/// The gates a rule declares, as written in config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Gates {
    /// Only files matching one of these are considered.
    pub path_matches: Vec<String>,
    /// Files matching any of these are skipped.
    pub path_not_matches: Vec<String>,
    /// Only files whose bytes contain **all** of these are parsed.
    pub file_contains: Vec<String>,
    /// Files whose bytes contain **any** of these are skipped.
    pub file_not_contains: Vec<String>,
}

impl Gates {
    /// Whether any gate is declared. A rule without gates parses every candidate file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.path_matches.is_empty()
            && self.path_not_matches.is_empty()
            && self.file_contains.is_empty()
            && self.file_not_contains.is_empty()
    }
}

/// A malformed gate pattern.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid `{field}` gate pattern `{pattern}`: {detail}")]
pub struct GateError {
    /// Which gate field it came from.
    pub field: &'static str,
    /// The pattern as written.
    pattern: String,
    /// What is wrong with it.
    detail: String,
}

/// Gates compiled for repeated evaluation.
#[derive(Debug)]
pub struct CompiledGates {
    path_matches: Option<GlobSet>,
    path_not_matches: Option<GlobSet>,
    file_contains: Vec<String>,
    file_not_contains: Vec<String>,
}

impl CompiledGates {
    /// Compile a rule's gates.
    ///
    /// # Errors
    ///
    /// Returns [`GateError`] for a malformed glob, naming the field it came from.
    pub fn compile(gates: &Gates) -> Result<Self, GateError> {
        Ok(Self {
            path_matches: compile_set(&gates.path_matches, "pathMatches")?,
            path_not_matches: compile_set(&gates.path_not_matches, "pathNotMatches")?,
            file_contains: gates.file_contains.clone(),
            file_not_contains: gates.file_not_contains.clone(),
        })
    }

    /// Whether this rule could match anything in a file at this path.
    ///
    /// Costs no file read, so it runs first.
    #[must_use]
    pub fn admits_path(&self, path: &FilePath) -> bool {
        let path = path.as_str();
        if self
            .path_not_matches
            .as_ref()
            .is_some_and(|set| set.is_match(path))
        {
            return false;
        }
        self.path_matches
            .as_ref()
            .is_none_or(|set| set.is_match(path))
    }

    /// Whether this rule could match anything in these bytes.
    ///
    /// Costs one read and a substring scan, and saves a parse. `memchr`'s searcher is used
    /// rather than a naive scan because this runs once per rule per file, and a file that
    /// no rule admits is the case worth making cheap.
    #[must_use]
    pub fn admits_content(&self, bytes: &[u8]) -> bool {
        for needle in &self.file_not_contains {
            if contains(bytes, needle.as_bytes()) {
                return false;
            }
        }
        // `all`, not `any`: a rule naming two strings needs both present, or it is
        // declaring something it does not mean.
        self.file_contains
            .iter()
            .all(|needle| contains(bytes, needle.as_bytes()))
    }

    /// Whether any content gate is declared, so a caller can skip reading when none is.
    #[must_use]
    pub fn has_content_gates(&self) -> bool {
        !self.file_contains.is_empty() || !self.file_not_contains.is_empty()
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    memchr::memmem::find(haystack, needle).is_some()
}

fn compile_set(patterns: &[String], field: &'static str) -> Result<Option<GlobSet>, GateError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| GateError {
            field,
            pattern: pattern.clone(),
            detail: e.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map(Some).map_err(|e| GateError {
        field,
        pattern: patterns.join(", "),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gates(
        path_matches: &[&str],
        path_not_matches: &[&str],
        file_contains: &[&str],
        file_not_contains: &[&str],
    ) -> CompiledGates {
        let own = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        CompiledGates::compile(&Gates {
            path_matches: own(path_matches),
            path_not_matches: own(path_not_matches),
            file_contains: own(file_contains),
            file_not_contains: own(file_not_contains),
        })
        .expect("compiles")
    }

    #[test]
    fn no_gates_admits_everything() {
        let g = gates(&[], &[], &[], &[]);
        assert!(g.admits_path(&FilePath::new("anything.ts")));
        assert!(g.admits_content(b""));
        assert!(g.admits_content(b"whatever"));
        assert!(!g.has_content_gates());
    }

    #[test]
    fn path_matches_narrows() {
        let g = gates(&["src/**/*.tsx"], &[], &[], &[]);
        assert!(g.admits_path(&FilePath::new("src/a/B.tsx")));
        assert!(!g.admits_path(&FilePath::new("src/a.ts")));
        assert!(!g.admits_path(&FilePath::new("lib/a.tsx")));
    }

    #[test]
    fn path_not_matches_wins_over_path_matches() {
        let g = gates(&["src/**"], &["**/generated/**"], &[], &[]);
        assert!(g.admits_path(&FilePath::new("src/a.ts")));
        assert!(!g.admits_path(&FilePath::new("src/generated/a.ts")));
    }

    #[test]
    fn file_contains_requires_all_of_them() {
        // `all`, not `any`. A rule naming two strings needs both, or it declared something
        // it did not mean and will parse files it cannot possibly match in.
        let g = gates(&[], &[], &["makeStyles", "theme"], &[]);
        assert!(g.admits_content(b"import { makeStyles } from 'x'; theme.spacing"));
        assert!(!g.admits_content(b"import { makeStyles } from 'x'"));
        assert!(!g.admits_content(b"theme.spacing"));
    }

    #[test]
    fn file_not_contains_rejects_on_any_of_them() {
        let g = gates(&[], &[], &[], &["@generated", "DO NOT EDIT"]);
        assert!(g.admits_content(b"ordinary source"));
        assert!(!g.admits_content(b"// @generated by something"));
        assert!(!g.admits_content(b"// DO NOT EDIT"));
    }

    #[test]
    fn content_gates_are_reported_so_a_caller_can_skip_the_read() {
        assert!(!gates(&["**/*.ts"], &[], &[], &[]).has_content_gates());
        assert!(gates(&[], &[], &["x"], &[]).has_content_gates());
        assert!(gates(&[], &[], &[], &["x"]).has_content_gates());
    }

    #[test]
    fn gates_only_ever_narrow() {
        // The property that keeps gates an optimization rather than a semantic. Whatever a
        // gated rule admits, an ungated one admits too — so removing a gate can never hide
        // a violation, only slow the run down.
        let ungated = gates(&[], &[], &[], &[]);
        let gated = gates(&["src/**"], &["**/gen/**"], &["needle"], &["skip"]);

        for path in ["src/a.ts", "src/gen/a.ts", "lib/a.ts"] {
            let path = FilePath::new(path);
            if gated.admits_path(&path) {
                assert!(
                    ungated.admits_path(&path),
                    "gating admitted more for {path}"
                );
            }
        }
        for bytes in [b"needle".as_slice(), b"skip needle", b"nothing"] {
            if gated.admits_content(bytes) {
                assert!(
                    ungated.admits_content(bytes),
                    "gating admitted more content"
                );
            }
        }
    }

    #[test]
    fn an_empty_needle_matches_anything() {
        // Degenerate but reachable from config, and it must not reject every file.
        let g = gates(&[], &[], &[""], &[]);
        assert!(g.admits_content(b""));
        assert!(g.admits_content(b"anything"));
    }

    #[test]
    fn content_matching_is_byte_exact() {
        let g = gates(&[], &[], &["makeStyles"], &[]);
        assert!(
            !g.admits_content(b"makestyles"),
            "matching must be case-sensitive"
        );
        assert!(g.admits_content("prefix makeStyles suffix".as_bytes()));
    }

    #[test]
    fn a_malformed_pattern_names_its_field() {
        let err = CompiledGates::compile(&Gates {
            path_matches: vec!["src/[".to_owned()],
            ..Gates::default()
        })
        .expect_err("malformed");
        assert_eq!(err.field, "pathMatches");

        let err = CompiledGates::compile(&Gates {
            path_not_matches: vec!["src/[".to_owned()],
            ..Gates::default()
        })
        .expect_err("malformed");
        assert_eq!(err.field, "pathNotMatches");
    }

    #[test]
    fn gates_report_whether_they_are_empty() {
        assert!(Gates::default().is_empty());
        assert!(
            !Gates {
                file_contains: vec!["x".to_owned()],
                ..Gates::default()
            }
            .is_empty()
        );
    }
}
