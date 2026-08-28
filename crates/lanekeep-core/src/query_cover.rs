//! The exact cover between a rule's declared languages and its per-language queries.
//!
//! One validation, shared by the two gates that enforce it — `lanekeep-config`'s
//! `build_rule` for an extracted TypeScript rule and `lanekeep-wasm`'s `validate_metadata`
//! for a component — so the two paths cannot drift in what they accept or in how they say
//! no. Each gate wraps [`QueryCoverProblem::describe`] in its own error type; the words are
//! shared, the types are not.
//!
//! Two checks are deliberately *not* here. An empty `languages` list has its own refusal in
//! both gates, older than this module and asserted by its own tests on each side. And the
//! per-entry "query text is empty" refusal belongs to `build_rule` alone: probe fixtures
//! answer `metadata` with an empty query on purpose, so the host gate admits one and the
//! config gate — the last gate before a rule runs — refuses it.

/// Why a rule's languages and queries do not cover each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryCoverProblem {
    /// The rule declares no query at all.
    NoQueries,
    /// Two queries name one language. Only one of them could ever run, and which one would
    /// be decided by position — the other is discarded with nothing reporting it.
    Duplicate {
        /// The language named twice.
        language: String,
    },
    /// A declared language has no query of its own, so the rule can never match on it.
    Missing {
        /// The language with no query.
        language: String,
    },
    /// A query names a language the rule does not target, so it can never run.
    Undeclared {
        /// The language the rule does not target.
        language: String,
    },
}

impl QueryCoverProblem {
    /// The refusal, phrased for the rule author, without the rule's id.
    ///
    /// Both gates prefix the id in their own error type; sharing the sentence is what keeps
    /// the component path and the TypeScript path saying the same thing for the same
    /// mistake.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::NoQueries => "declares no query for any language — a rule with no query \
                                can never match, and silently"
                .to_owned(),
            Self::Duplicate { language } => format!(
                "declares two queries for `{language}` — only one of them could run, and \
                 the other would be discarded silently"
            ),
            Self::Missing { language } => format!(
                "declares no query for `{language}` — a rule runs only on files whose \
                 language it names, so a language with no query can never match"
            ),
            Self::Undeclared { language } => format!(
                "declares a query for `{language}`, which it does not target — that query \
                 can never run, and nothing would report the mistake"
            ),
        }
    }
}

/// Check that the query entries exactly cover `languages`, with no language named twice.
///
/// `queries` is the entry languages in declaration order. When several problems exist the
/// first in check order — no queries, a duplicate, a missing language, an undeclared one —
/// is named, and within a check the first offender in declaration order, so two runs over
/// one rule report the same refusal.
///
/// # Errors
///
/// The first [`QueryCoverProblem`] found, in the order above.
pub fn check<'a, I>(languages: &[String], queries: I) -> Result<(), QueryCoverProblem>
where
    I: IntoIterator<Item = &'a str>,
{
    let entries: Vec<&str> = queries.into_iter().collect();

    if entries.is_empty() {
        return Err(QueryCoverProblem::NoQueries);
    }

    let mut seen: Vec<&str> = Vec::with_capacity(entries.len());
    for language in &entries {
        if seen.contains(language) {
            return Err(QueryCoverProblem::Duplicate {
                language: (*language).to_owned(),
            });
        }
        seen.push(language);
    }

    for language in languages {
        if !entries.contains(&language.as_str()) {
            return Err(QueryCoverProblem::Missing {
                language: language.clone(),
            });
        }
    }

    for language in &entries {
        if !languages.iter().any(|declared| declared == language) {
            return Err(QueryCoverProblem::Undeclared {
                language: (*language).to_owned(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn languages(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn an_exact_cover_passes() {
        assert_eq!(
            check(
                &languages(&["typescript", "python"]),
                ["typescript", "python"]
            ),
            Ok(())
        );
    }

    #[test]
    fn entry_order_does_not_matter_for_a_cover() {
        assert_eq!(
            check(
                &languages(&["typescript", "python"]),
                ["python", "typescript"]
            ),
            Ok(())
        );
    }

    #[test]
    fn no_queries_at_all_is_refused() {
        assert_eq!(
            check(&languages(&["rust"]), []),
            Err(QueryCoverProblem::NoQueries)
        );
    }

    #[test]
    fn a_language_named_twice_is_refused_naming_it() {
        // The silent failure this exists to close: both cover directions hold — every
        // declared language has an entry, every entry names a declared language — and one
        // of the two queries would be discarded by position.
        assert_eq!(
            check(&languages(&["rust"]), ["rust", "rust"]),
            Err(QueryCoverProblem::Duplicate {
                language: "rust".to_owned()
            })
        );
    }

    #[test]
    fn a_declared_language_without_a_query_is_refused_naming_it() {
        assert_eq!(
            check(&languages(&["rust", "go"]), ["rust"]),
            Err(QueryCoverProblem::Missing {
                language: "go".to_owned()
            })
        );
    }

    #[test]
    fn a_query_for_an_undeclared_language_is_refused_naming_it() {
        assert_eq!(
            check(&languages(&["rust"]), ["rust", "go"]),
            Err(QueryCoverProblem::Undeclared {
                language: "go".to_owned()
            })
        );
    }

    #[test]
    fn the_first_problem_in_declaration_order_is_the_one_named() {
        // Deterministic refusals: a rule with two duplicates names the first.
        assert_eq!(
            check(&languages(&["a", "b"]), ["b", "b", "a", "a"]),
            Err(QueryCoverProblem::Duplicate {
                language: "b".to_owned()
            })
        );
    }

    #[test]
    fn every_problem_describes_itself_without_the_id() {
        // The gates prefix `\`{id}\` ` themselves; a description starting with the verb is
        // what keeps that composition grammatical on both sides.
        for problem in [
            QueryCoverProblem::NoQueries,
            QueryCoverProblem::Duplicate {
                language: "x".to_owned(),
            },
            QueryCoverProblem::Missing {
                language: "x".to_owned(),
            },
            QueryCoverProblem::Undeclared {
                language: "x".to_owned(),
            },
        ] {
            assert!(problem.describe().starts_with("declares"), "{problem:?}");
        }
    }
}
