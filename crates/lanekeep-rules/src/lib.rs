//! Built-in rules shipped with lanekeep.
//!
//! The rules shipping with lanekeep, authored in TypeScript against the same host API that
//! project-authored rules use, embedded into the binary at build time.
//!
//! Built-ins deliberately get no privileged path into the engine. Rules dogfooding the
//! public API is the strongest available evidence that the API is sufficient for real work
//! — if a built-in needed something a project rule cannot have, the API would be wrong.
//!
//! # How they are reached
//!
//! A config imports them by specifier:
//!
//! ```ts
//! import noDefaultExport from 'lanekeep/no-default-export'
//! import noRestrictedImports from 'lanekeep/no-restricted-imports'
//!
//! export default defineConfig({
//!   rules: [
//!     noDefaultExport,
//!     noRestrictedImports({ restrictions: [{ module: 'lodash/*' }] }),
//!   ],
//! })
//! ```
//!
//! The module loader resolves the `lanekeep/` prefix to the sources embedded here, so
//! nothing is written to disk and a built-in cannot be shadowed by a file in the project.

/// The rules this build ships, as `(name, source)`.
///
/// Ordered, so anything derived from this list — a `--help` listing, the ruleset hash —
/// does not depend on declaration order changing under an edit.
const BUILT_INS: &[(&str, &str)] = &[
    (
        "no-default-export",
        include_str!("../rules/no-default-export.ts"),
    ),
    (
        "no-restricted-imports",
        include_str!("../rules/no-restricted-imports.ts"),
    ),
];

/// The source of a built-in rule, given the part of the specifier after `lanekeep/`.
///
/// Returns `None` for anything else, so an unknown built-in surfaces as an unresolved
/// import naming the specifier rather than as a rule that silently never runs.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    BUILT_INS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, source)| *source)
}

/// Every built-in rule's name, in a stable order.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUILT_INS.iter().map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_is_reachable_by_name() {
        for name in names() {
            assert!(
                source(name).is_some(),
                "`{name}` is listed but has no source"
            );
        }
    }

    #[test]
    fn an_unknown_name_resolves_to_nothing() {
        // So a typo surfaces as an unresolved import naming the specifier, rather than as
        // a rule that quietly never runs.
        assert_eq!(source("no-such-rule"), None);
        assert_eq!(source(""), None);
        assert_eq!(source("no-default-export.ts"), None);
    }

    #[test]
    fn names_are_stable_and_unique() {
        let all: Vec<&str> = names().collect();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "duplicate built-in name");
        assert_eq!(names().collect::<Vec<_>>(), all, "order must be stable");
    }

    #[test]
    fn every_source_declares_a_namespaced_id() {
        // The ids are what appear in suppression comments and config. A built-in shipping
        // under `local/` would be indistinguishable from a project rule.
        for name in names() {
            let source = source(name).unwrap_or_default();
            assert!(
                source.contains(&format!("id: 'lanekeep/{name}'")),
                "`{name}` does not declare the id its specifier implies"
            );
        }
    }

    #[test]
    fn every_source_imports_only_the_host_module() {
        // Built-ins get no privileged path. If one imported something a project rule
        // cannot, it would stop being evidence that the public API is sufficient.
        for name in names() {
            let source = source(name).unwrap_or_default();
            for line in source
                .lines()
                .filter(|l| l.trim_start().starts_with("import "))
            {
                assert!(
                    line.contains("from 'lanekeep'"),
                    "`{name}` imports something other than the host module: {line}"
                );
            }
        }
    }
}
