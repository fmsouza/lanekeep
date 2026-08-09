//! Built-in rules shipped with lanekeep.
//!
//! The rules shipping with lanekeep, authored against the same host API that project-authored
//! rules use and embedded into the binary at build time. Most are TypeScript modules; the two
//! that check Rust are WebAssembly components built from `rust-rules/`.
//!
//! Built-ins deliberately get no privileged path into the engine. Rules dogfooding the
//! public API is the strongest available evidence that the API is sufficient for real work
//! — if a built-in needed something a project rule cannot have, the API would be wrong.
//!
//! # How they are reached
//!
//! By specifier, and the specifier does not say which kind a rule is. From a `lanekeep.json`,
//! which is what `lanekeep init` writes and the only format that can name either kind:
//!
//! ```json
//! {
//!   "rules": [
//!     "lanekeep/no-default-export",
//!     "lanekeep/no-unwrap",
//!     { "rule": "lanekeep/no-restricted-imports",
//!       "options": { "restrictions": [{ "module": "lodash/*" }] } }
//!   ]
//! }
//! ```
//!
//! **Which table a rule is in is not something a config writes**, and that is the property
//! worth protecting: a rule migrating from TypeScript to Rust must not require anybody to edit
//! a config. `lanekeep_js::RuleRoot` carries both lookups so that `lanekeep/<name>` means one
//! thing everywhere — the module loader serves [`source`], `lanekeep-config` resolves
//! [`component`], and neither can decide differently from the other.
//!
//! A `lanekeep.config.ts` reaches the TypeScript ones the way it always has:
//!
//! ```ts
//! import noDefaultExport from 'lanekeep/no-default-export'
//!
//! export default defineConfig({ rules: [noDefaultExport] })
//! ```
//!
//! **It cannot reach a component one.** A component is not a value a module can import: it has
//! no JavaScript to evaluate, and its identity comes from its own `metadata` export rather than
//! from a `defineRule` call the sandbox could read. The resolver refuses such an import as
//! itself — `lanekeep_js::ResolveError::NotAModule`, naming the format that can reach it —
//! rather than as a rule that does not exist.
//!
//! Nothing here is written to disk in either case, and a built-in cannot be shadowed by a file
//! in the project.
//!
//! The same prefix also serves shared modules — `lanekeep/paths` and friends — which are
//! not rules and are listed separately for that reason.

/// The rules this build ships as TypeScript, as `(name, source)`.
///
/// Ordered, so the source stays greppable and a diff shows what moved. Nothing derives an
/// order from this table directly — see [`names`], which merges both tables and sorts.
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

/// The rules this build ships as components, as `(name, bytes)`.
///
/// **A rule is in exactly one of the two tables.** For the length of a migration it was in
/// both — the TypeScript source was what a config resolved and the component was what the
/// expectation table held to the same assertions — and that state is over: the two rules here
/// are what a config resolves, and their TypeScript originals are deleted. A name appearing in
/// both tables would be two programs answering to one id, with nothing to say which one ran.
///
/// Built from `rust-rules/<name>/` by `just rust-rules`, which is also what copies the artifact
/// here. The bytes are committed, so the gate needs neither `cargo component` nor a wasm target.
///
/// Ordered, on the same terms as [`BUILT_IN_RULES`].
const BUILT_IN_COMPONENTS: &[(&str, &[u8])] = &[
    (
        "no-glob-import",
        include_bytes!("../components/no-glob-import.wasm"),
    ),
    ("no-unwrap", include_bytes!("../components/no-unwrap.wasm")),
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

/// The component behind a built-in rule's name, or `None` for one that has none.
///
/// Bytes rather than a path, on the same terms as [`source`]: a built-in is embedded in the
/// binary, so nothing is written to disk and no file in the project can shadow it.
#[must_use]
pub fn component(name: &str) -> Option<&'static [u8]> {
    BUILT_IN_COMPONENTS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, bytes)| *bytes)
}

/// Every built-in rule's name, in a stable order.
///
/// Rules only. A shared module is not a rule, and listing one as though it were would
/// promise a card and an id it does not have.
///
/// **Both tables, sorted rather than concatenated.** Which table a rule is in is an authoring
/// detail, so a listing that grouped by it would put the Rust rules last today and somewhere
/// else after the next migration — a rule appearing to move because someone rewrote it is
/// exactly the kind of churn a stable order exists to prevent. Sorting also makes the order
/// independent of declaration order in either table, which is stronger than the convention
/// that they are each kept alphabetical.
pub fn names() -> impl Iterator<Item = &'static str> {
    let mut all: Vec<&'static str> = BUILT_IN_RULES
        .iter()
        .map(|(name, _)| *name)
        .chain(BUILT_IN_COMPONENTS.iter().map(|(name, _)| *name))
        .collect();
    all.sort_unstable();
    all.into_iter()
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
        // Reachable *and* reachable as exactly one thing. `source` and `component` are asked
        // separately by separate crates — the module loader asks one, `lanekeep-config` asks
        // the other — and neither is in a position to notice that the other also answered.
        for name in names() {
            match (source(name).is_some(), component(name).is_some()) {
                (true, false) | (false, true) => {}
                (false, false) => panic!("`{name}` is listed but has neither source nor component"),
                (true, true) => panic!(
                    "`{name}` is listed as both a module and a component — two programs \
                     answering to one id, with nothing to say which one ran"
                ),
            }
        }
    }

    #[test]
    fn the_migrated_rules_are_components_and_not_modules() {
        // The swap itself, named. The generic tests above hold whichever table a rule is in,
        // which is what makes them survive a migration — and is also what would let both of
        // these quietly revert to TypeScript with nothing red.
        for name in ["no-unwrap", "no-glob-import"] {
            assert!(
                component(name).is_some(),
                "`{name}` ships as a component and does not"
            );
            assert_eq!(
                source(name),
                None,
                "`{name}`'s TypeScript original is deleted and must not resolve"
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
    fn every_component_is_a_rule_that_ships() {
        // A component whose name is not a built-in is a rule nothing can reach: `component` is
        // looked up by the same name `source` is, so a mismatch is an artifact embedded in the
        // binary and never executed.
        for (name, _) in BUILT_IN_COMPONENTS {
            assert!(
                names().any(|rule| rule == *name),
                "`{name}` has a component but is not a built-in rule"
            );
        }
    }

    #[test]
    fn a_component_is_webassembly_rather_than_a_placeholder() {
        // `include_bytes!` of a stub or a half-written file compiles, and the failure would be a
        // load error inside whichever test ran first. Four bytes are enough to tell them apart.
        for (name, bytes) in BUILT_IN_COMPONENTS {
            assert_eq!(
                bytes.get(..4),
                Some(b"\0asm".as_slice()),
                "`{name}`'s component does not begin with the WebAssembly magic"
            );
        }
    }

    #[test]
    fn a_rule_without_a_component_has_none() {
        assert_eq!(component("no-such-rule"), None);
        // Still authored in TypeScript, and asked by name rather than assumed: this is what
        // distinguishes "not migrated" from "migrated and the table was not updated".
        assert_eq!(component("no-default-export"), None);
    }

    #[test]
    fn names_are_stable_and_unique() {
        // Strictly ascending, which says uniqueness and order in one assertion — and says it
        // about the *merge*, which is where a duplicate across the two tables would appear.
        let all: Vec<&str> = names().collect();
        assert!(
            all.windows(2).all(|pair| pair[0] < pair[1]),
            "built-in names must be unique and ascending: {all:?}"
        );
        assert_eq!(names().collect::<Vec<_>>(), all, "order must be stable");
        assert_eq!(
            all.len(),
            BUILT_IN_RULES.len() + BUILT_IN_COMPONENTS.len(),
            "every rule in either table must be listed"
        );
    }

    #[test]
    fn both_tables_are_kept_in_order() {
        // `names` sorts, so this is about the source rather than about behavior: a table read
        // top to bottom should be the list a reader expects, and an entry appended in the wrong
        // place is invisible once the output is sorted.
        for table in [
            BUILT_IN_RULES.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            BUILT_IN_COMPONENTS
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>(),
        ] {
            assert!(
                table.windows(2).all(|pair| pair[0] < pair[1]),
                "table is out of order: {table:?}"
            );
        }
    }

    #[test]
    fn every_rule_declares_the_id_its_specifier_implies() {
        // The ids are what appear in suppression comments and config. A built-in shipping
        // under `local/` would be indistinguishable from a project rule.
        //
        // **The two halves are not equally strong, and the weaker one is still worth having.**
        // A module's id is a literal in source, so the check is exact. A component answers its
        // id from `metadata`, which cannot be read without instantiating it — this crate has no
        // wasm runtime — so what is checked is that the id appears in the bytes at all. That
        // fails on the mistake it is written for: a component copied from the wrong crate, or
        // built before an id was corrected. The exact claim is made where a runtime exists —
        // `tests/no_unwrap.rs` and `tests/no_glob_import.rs` pin `rule_id` on every violation
        // of every case, through the real engine.
        for name in names() {
            let id = format!("lanekeep/{name}");
            if let Some(source) = source(name) {
                assert!(
                    source.contains(&format!("id: '{id}'")),
                    "`{name}` does not declare the id its specifier implies"
                );
            } else {
                let bytes = component(name).unwrap_or_default();
                assert!(
                    bytes
                        .windows(id.len())
                        .any(|window| window == id.as_bytes()),
                    "`{name}`'s component does not contain the id its specifier implies"
                );
            }
        }
    }

    #[test]
    fn every_source_imports_only_what_a_project_rule_could() {
        // Built-ins get no privileged path. If one imported something a project rule
        // cannot, it would stop being evidence that the public API is sufficient — so
        // every import here has to be one a config could write verbatim.
        //
        // Source-backed rules only, because an import statement is what this reads. The same
        // claim about a component is about its *instance imports*, and it is made where a
        // component can be inspected: `crates/lanekeep-wasm/tests/world_shape.rs`'s
        // `no_shipped_rule_component_imports_ambient_authority` runs the engine's own filter
        // over every artifact in `components/` and holds it to the declared world.
        for name in names().chain(module_names()) {
            let Some(source) = source(name) else {
                continue;
            };
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
        // A shared module has none of those. Whichever form a rule is authored in, it has to
        // declare itself as one — `defineRule` for a module, and for a component the fact that
        // it is a component at all, since a `rule` world is the only thing that can be loaded.
        for name in names() {
            if let Some(source) = source(name) {
                assert!(
                    source.contains("defineRule"),
                    "`{name}` is listed as a rule but does not define one"
                );
            } else {
                assert!(
                    component(name).is_some(),
                    "`{name}` is listed as a rule and is neither a module nor a component"
                );
            }
        }
    }
}
