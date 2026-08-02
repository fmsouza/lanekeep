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
//!
//! The same prefix also serves shared modules — `lanekeep/paths` and friends — which are
//! not rules and are listed separately for that reason.

/// The rules this build ships, as `(name, source)`.
///
/// Ordered, so anything derived from this list — a `--help` listing, the ruleset hash —
/// does not depend on declaration order changing under an edit.
const BUILT_IN_RULES: &[(&str, &str)] = &[
    (
        "no-broad-except",
        include_str!("../rules/no-broad-except.ts"),
    ),
    (
        "no-circular-imports",
        include_str!("../rules/no-circular-imports.ts"),
    ),
    (
        "no-context-in-struct",
        include_str!("../rules/no-context-in-struct.ts"),
    ),
    (
        "no-default-export",
        include_str!("../rules/no-default-export.ts"),
    ),
    (
        "no-mutable-default-argument",
        include_str!("../rules/no-mutable-default-argument.ts"),
    ),
    (
        "no-package-init",
        include_str!("../rules/no-package-init.ts"),
    ),
    (
        "no-restricted-imports",
        include_str!("../rules/no-restricted-imports.ts"),
    ),
    (
        "no-unused-exports",
        include_str!("../rules/no-unused-exports.ts"),
    ),
];

/// Shared modules the built-in rules import, and project rules may too.
///
/// Separate from the rules because they are not rules: they have no id, no card and no
/// query, so anything that lists rules must not list them. They resolve through the same
/// `lanekeep/` prefix, which is deliberate — a helper two built-ins both need is a helper a
/// project rule will need eventually, and there is nothing to gain from hiding it.
const BUILT_IN_MODULES: &[(&str, &str)] = &[("paths", include_str!("../modules/paths.ts"))];

/// The source behind a `lanekeep/<name>` specifier, rule or shared module.
///
/// Returns `None` for anything else, so an unknown built-in surfaces as an unresolved
/// import naming the specifier rather than as a rule that silently never runs.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    BUILT_IN_RULES
        .iter()
        .chain(BUILT_IN_MODULES)
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, source)| *source)
}

/// Every built-in rule's name, in a stable order.
///
/// Rules only. A shared module is not a rule, and listing one as though it were would
/// promise a card and an id it does not have.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUILT_IN_RULES.iter().map(|(name, _)| *name)
}

/// Every shared module's name, in a stable order.
pub fn module_names() -> impl Iterator<Item = &'static str> {
    BUILT_IN_MODULES.iter().map(|(name, _)| *name)
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
    fn every_source_imports_only_what_a_project_rule_could() {
        // Built-ins get no privileged path. If one imported something a project rule
        // cannot, it would stop being evidence that the public API is sufficient — so
        // every import here has to be one a config could write verbatim.
        for name in names().chain(module_names()) {
            let source = source(name).unwrap_or_default();
            for line in source
                .lines()
                .filter(|l| l.trim_start().starts_with("import "))
            {
                let reachable = line.contains("from 'lanekeep'")
                    || module_names().any(|m| line.contains(&format!("from 'lanekeep/{m}'")));
                assert!(
                    reachable,
                    "`{name}` imports something a project rule could not: {line}"
                );
            }
        }
    }

    #[test]
    fn shared_modules_are_reachable_but_are_not_rules() {
        for name in module_names() {
            assert!(
                source(name).is_some(),
                "`{name}` is listed but has no source"
            );
            assert!(
                !names().any(|rule| rule == name),
                "`{name}` is listed as both a module and a rule"
            );
        }
        assert!(
            source("paths").is_some(),
            "the shared path helpers must resolve"
        );
    }

    #[test]
    fn listing_rules_does_not_list_modules() {
        // `lanekeep rules` renders an id, a severity and a card for everything it lists.
        // A shared module has none of those.
        for name in names() {
            assert!(
                source(name).unwrap_or_default().contains("defineRule"),
                "`{name}` is listed as a rule but does not define one"
            );
        }
    }
}
