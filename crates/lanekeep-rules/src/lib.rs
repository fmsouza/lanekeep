//! Built-in rules shipped with lanekeep.
//!
//! The rules shipping with lanekeep, authored against the same host API that project-authored
//! rules use and embedded into the binary at build time. Ten are TypeScript modules evaluated
//! in QuickJS — the two Python-targeting rules and the eight TypeScript-targeting rules, four of
//! which were briefly compiled to a StarlingMonkey component and reverted, because that form cost
//! 13 MB and 110× per host-API crossing for no speed benefit. The rest are WebAssembly
//! components —
//! two built from `rust-rules/` and two authored in Go into one shared component — where the
//! component path is small (100 KB or less each) and fast (1.1× a QuickJS crossing).
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
//! worth protecting: a rule migrating from TypeScript to a component must not require anybody to
//! edit a config. `lanekeep_js::RuleRoot` carries both lookups so that `lanekeep/<name>` means
//! one thing everywhere — the module loader serves [`source`], `lanekeep-config` resolves
//! [`component`], and neither can decide differently from the other.
//!
//! A `lanekeep.config.ts` reaches the ones that still ship as modules the way it always has:
//!
//! ```ts
//! import noBroadExcept from 'lanekeep/no-broad-except'
//!
//! export default defineConfig({ rules: [noBroadExcept] })
//! ```
//!
//! **It cannot reach a component one.** A component is not a value a module can import: it has
//! no JavaScript to evaluate, and its identity comes from its own `metadata` export rather than
//! from a `defineRule` call the sandbox could read. The resolver refuses such an import as
//! itself — `lanekeep_js::ResolveError::NotAModule`, naming the format that can reach it —
//! rather than as a rule that does not exist.
//!
//! **That refusal is asked before the source lookup, and the order is the whole of the
//! guarantee.** A component rule has no TypeScript source here — its Rust or Go *is* the
//! source, and its `.ts` was deleted. A resolver that answered from the source first for a
//! name that had both would let `lanekeep.config.ts` run the QuickJS copy while `lanekeep.json`
//! ran the component — one id, two programs, and nothing in the output to say which one
//! reported. No shipped rule is in both tables today: the TypeScript rules are modules and the
//! Rust/Go rules are components, with no overlap.
//!
//! Nothing here is written to disk in either case, and a built-in cannot be shadowed by a file
//! in the project.
//!
//! The same prefix also serves shared modules — `lanekeep/paths` and friends — which are
//! not rules and are listed separately for that reason.

/// The rules this build runs as TypeScript modules, as `(name, source)`.
///
/// Evaluated in QuickJS, from source, on every run. Ten rules ship this way: the two
/// Python-targeting rules (`no-broad-except`, `no-mutable-default-argument`) and the eight
/// TypeScript-targeting rules (`duplicate-implementation`, `no-assertionless-test`,
/// `no-circular-imports`, `no-default-export`, `no-restricted-calls`, `no-restricted-imports`,
/// `no-restricted-types`, `no-unused-exports`). Four of them were
/// briefly compiled ahead of time into a shared StarlingMonkey component and reverted, because
/// the compiled form cost 13 MB of binary and 110× per host-API crossing for no speed benefit —
/// see `docs/architecture.md` §15.1 for the measurement.
///
/// Ordered, so the source stays greppable and a diff shows what moved. Nothing derives an
/// order from this table directly — see [`names`], which merges the tables and sorts.
const BUILT_IN_RULES: &[(&str, &str)] = &[
    (
        "duplicate-implementation",
        include_str!("../rules/duplicate-implementation.ts"),
    ),
    (
        "no-assertionless-test",
        include_str!("../rules/no-assertionless-test.ts"),
    ),
    (
        "no-broad-except",
        include_str!("../rules/no-broad-except.ts"),
    ),
    (
        "no-circular-imports",
        include_str!("../rules/no-circular-imports.ts"),
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
        "no-restricted-calls",
        include_str!("../rules/no-restricted-calls.ts"),
    ),
    (
        "no-restricted-imports",
        include_str!("../rules/no-restricted-imports.ts"),
    ),
    (
        "no-restricted-types",
        include_str!("../rules/no-restricted-types.ts"),
    ),
    (
        "no-unused-exports",
        include_str!("../rules/no-unused-exports.ts"),
    ),
];

/// The components this build ships, as `(component name, bytes)`.
///
/// **Keyed by the component rather than by a rule**, because a component hosts one rule or
/// several and the artifact is what is embedded. `go-builtins` hosts two; the two built from
/// `rust-rules/<name>/` host one each and are named after it.
///
/// A rule is authored once and runs once — [`COMPONENT_RULES`] and [`BUILT_IN_RULES`] are
/// disjoint, and a name in both would be two programs answering to one id with nothing to say
/// which one ran.
///
/// Built by `just rust-rules` and `just go-rules`, which are also what copy the artifacts
/// here. The bytes are committed, so the gate needs none of `cargo component`, a wasm target,
/// Go or TinyGo.
///
/// One shipped component: its name, its bytes, and its source map if it has one.
///
/// Named so the two tables below and the lookup behind them read as rows rather than as tuples
/// — and because a three-element tuple of two slices and an optional slice is a shape clippy is
/// right to object to.
type BuiltInComponent = (&'static str, &'static [u8], Option<&'static [u8]>);

/// # The third column is the component's source map, and it is optional
///
/// `None` for every shipped component, because the two built from Rust and the one built from
/// Go all fail by panicking, which traps, and a trap arrives at the host with no stack at all.
/// There is nothing to remap. `go-rules/` builds with `-panic=trap` and `-no-debug`, so there is
/// not even a name inside the artifact to map back to. A source map would only be needed for a
/// component compiled from TypeScript, and none ships today — the eight TypeScript-targeting
/// built-ins run as QuickJS modules (see [`BUILT_IN_RULES`]).
///
/// Ordered, on the same terms as [`BUILT_IN_RULES`].
const BUILT_IN_COMPONENTS: &[BuiltInComponent] = &[
    (
        "go-builtins",
        include_bytes!("../components/go-builtins.wasm"),
        None,
    ),
    (
        "no-glob-import",
        include_bytes!("../components/no-glob-import.wasm"),
        None,
    ),
    (
        "no-unwrap",
        include_bytes!("../components/no-unwrap.wasm"),
        None,
    ),
];

/// Which component hosts a rule, and at which index.
///
/// An explicit table rather than a lookup through the component, because resolving a
/// specifier must not require executing guest code. A test asserts it against the
/// component's own `rules()` output, so a table that drifts fails the gate rather than
/// dispatching a config to the wrong rule.
///
/// The index is the position a rule sits at in its component's own enumeration, which for
/// `typescript-builtins` is the order `crates/lanekeep-rules/typescript/entry.ts` passes to
/// `register` and for `go-builtins` is the order `go-rules/main.go`'s `ruleset` declares.
/// Inserting a rule in the middle of either renumbers every rule after it, so a new one goes
/// wherever the alphabet puts it and this table is re-recorded.
///
/// Ordered by rule name, on the same terms as [`BUILT_IN_RULES`] — a reader looking a rule up
/// is looking up a name, and the component column is what they learn.
const COMPONENT_RULES: &[(&str, &str, u32)] = &[
    ("no-context-in-struct", "go-builtins", 0),
    ("no-glob-import", "no-glob-import", 0),
    ("no-package-init", "go-builtins", 1),
    ("no-unwrap", "no-unwrap", 0),
];

/// Shared modules the built-in rules import, and project rules may too.
///
/// Separate from the rules because they are not rules: they have no id, no card and no
/// query, so anything that lists rules must not list them. They resolve through the same
/// `lanekeep/` prefix, which is deliberate — a helper two built-ins both need is a helper a
/// project rule will need eventually, and there is nothing to gain from hiding it.
const BUILT_IN_MODULES: &[(&str, &str)] = &[
    ("paths", include_str!("../modules/paths.ts")),
    ("patterns", include_str!("../modules/patterns.ts")),
];

/// The TypeScript behind a `lanekeep/<name>`, rule or shared module.
///
/// Returns `None` for anything else, so an unknown built-in surfaces as an unresolved
/// import naming the specifier rather than as a rule that silently never runs.
///
/// **Answering here does not make a name importable from a `lanekeep.config.ts`.**
/// `lanekeep_js::RuleRoot::resolve` asks [`component`] first and refuses a name it
/// answers, so a component rule's TypeScript source — which no longer exists for rules
/// ported to Rust or Go — is never served to the sandbox. A module rule's source is what
/// the sandbox evaluates; a component rule has no source at all.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    BUILT_IN_RULES
        .iter()
        .chain(BUILT_IN_MODULES)
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, source)| *source)
}

/// The component behind a built-in rule's name and the index it sits at, or `None`.
///
/// Bytes rather than a path, on the same terms as [`source`]: a built-in is embedded in the
/// binary, so nothing is written to disk and no file in the project can shadow it.
///
/// **The index is half the answer and not a detail.** One artifact hosts several rules, so
/// bytes alone name a program rather than a rule — a caller handed only the bytes would run
/// whichever rule the component happens to enumerate first, and for the Go built-ins that
/// is a different rule than the one the config named.
#[must_use]
pub fn component(name: &str) -> Option<(&'static [u8], u32)> {
    let ((_, bytes, _), index) = hosted(name)?;
    Some((bytes, index))
}

/// The source map of the component behind a built-in rule's name, or `None`.
///
/// Keyed by the *rule* rather than by the component, so a caller asks it with the same name it
/// asks [`component`] with — a config names a rule, and which artifact hosts it is not something
/// a config knows.
///
/// **What a missing map costs is a diagnostic and nothing else.** A rule that throws is reported
/// at a position in the program that actually ran; with the map, that position is translated back
/// into the file its author edited. A violation's position never passes through here — the host
/// reads it from the parse tree — so nothing about what a rule *finds* depends on this answer.
#[must_use]
pub fn component_source_map(name: &str) -> Option<&'static [u8]> {
    hosted(name)?.0.2
}

/// Whether a name is declared as a component rule, regardless of whether its host ships.
///
/// The distinction from [`component`] is the point. `component` answers `None` for two
/// different facts: a name that is not a component at all, and a name whose
/// `COMPONENT_RULES` row names a host this build does not ship. The first is a typo; the
/// second is a broken table — a lanekeep bug, not a misspelling — and only this lookup can
/// tell them apart.
///
/// `lanekeep_js::RuleRoot` asks this after [`component`] and before [`source`], so a name
/// that is declared as a component but whose host is missing is refused as a broken table
/// rather than silently served from its (stale) TypeScript source.
#[must_use]
pub fn is_declared_component(name: &str) -> bool {
    is_declared_component_in(COMPONENT_RULES, name)
}

/// The generic half of [`is_declared_component`], split out so a test can build a broken table.
fn is_declared_component_in(rules: &[(&str, &str, u32)], name: &str) -> bool {
    rules.iter().any(|(candidate, _, _)| *candidate == name)
}

/// The row of [`BUILT_IN_COMPONENTS`] that hosts a rule, and the index it sits at.
///
/// One lookup behind both accessors, so a rule cannot be found by one and missed by the other.
fn hosted(name: &str) -> Option<(&'static BuiltInComponent, u32)> {
    hosted_in(COMPONENT_RULES, BUILT_IN_COMPONENTS, name)
}

/// The generic half of [`hosted`], split out so a test can build a broken table.
///
/// The broken-table state — a `rules` row whose host is absent from `components` — is not
/// constructible from the `const` tables, which the gate holds consistent; a test that needs
/// to see it passes its own.
fn hosted_in<'a>(
    rules: &[(&str, &str, u32)],
    components: &'a [BuiltInComponent],
    name: &str,
) -> Option<(&'a BuiltInComponent, u32)> {
    let (_, host, index) = rules.iter().find(|(candidate, _, _)| *candidate == name)?;
    let component = components
        .iter()
        .find(|(candidate, _, _)| candidate == host)?;
    Some((component, *index))
}

/// Every built-in rule's name, in a stable order.
///
/// Rules only. A shared module is not a rule, and listing one as though it were would
/// promise a card and an id it does not have.
///
/// **Both tables, sorted rather than concatenated.** Which table a rule is in is an authoring
/// detail, so a listing that grouped by it would put the component rules last today and
/// somewhere else after the next migration — a rule appearing to move because someone rewrote
/// it is exactly the kind of churn a stable order exists to prevent. Sorting also makes the
/// order independent of declaration order in either table, which is stronger than the
/// convention that they are each kept alphabetical.
pub fn names() -> impl Iterator<Item = &'static str> {
    let mut all: Vec<&'static str> = BUILT_IN_RULES
        .iter()
        .map(|(name, _)| *name)
        .chain(COMPONENT_RULES.iter().map(|(name, _, _)| *name))
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
        // Reachable *and* reachable as exactly one program. `source` and `component` are asked
        // separately by separate crates — the module loader asks one, `lanekeep-config` asks
        // the other — and neither is in a position to notice that the other also answered.
        //
        // The question is which table a rule *runs* from, and that is `BUILT_IN_RULES` against
        // `COMPONENT_RULES`: a module rule runs from `BUILT_IN_RULES`, a component rule from
        // `COMPONENT_RULES`, and never both.
        for name in names() {
            let module = BUILT_IN_RULES.iter().any(|(n, _)| *n == name);
            match (module, component(name).is_some()) {
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
    fn component_slots_are_unique() {
        // The pairing is one-to-one in the direction that matters for dispatch: no two
        // rules may claim the same slot of the same component, or one config entry would
        // silently run the other rule.
        let mut slots: Vec<(&str, u32)> = COMPONENT_RULES
            .iter()
            .map(|(_, host, index)| (*host, *index))
            .collect();
        slots.sort_unstable();
        let before = slots.len();
        slots.dedup();
        assert_eq!(before, slots.len(), "two rules claim one slot: {slots:?}");
    }

    #[test]
    fn the_rust_and_go_rules_are_components_and_the_ts_rules_are_modules() {
        // The swap itself, named. The generic tests above hold whichever table a rule is in,
        // which is what makes them survive a migration — and is also what would let any of
        // these quietly revert with nothing red.
        //
        // Two shapes, deliberately together. The four rules ported to another language have no
        // source at all and ship as components; the eight TypeScript rules ship as modules,
        // evaluated in QuickJS, because the compiled-component form (StarlingMonkey in WASM)
        // costs 13 MB and 110× per crossing for no speed benefit.
        for name in [
            "no-context-in-struct",
            "no-glob-import",
            "no-package-init",
            "no-unwrap",
        ] {
            assert!(
                component(name).is_some(),
                "`{name}` ships as a component and does not"
            );
            assert_eq!(
                source(name),
                None,
                "`{name}` is authored in the language it inspects, so its TypeScript original is \
                 deleted and must not resolve — a `.ts` still answering here would be tied to no \
                 build and free to drift from the rule that ships"
            );
        }

        for name in [
            "duplicate-implementation",
            "no-assertionless-test",
            "no-circular-imports",
            "no-default-export",
            "no-restricted-calls",
            "no-restricted-imports",
            "no-restricted-types",
            "no-unused-exports",
        ] {
            assert!(
                component(name).is_none(),
                "`{name}` ships as a module and should not also be a component"
            );
            assert!(
                BUILT_IN_RULES.iter().any(|(n, _)| *n == name),
                "`{name}` is a TypeScript built-in and should be in the table the sandbox \
                 evaluates from"
            );
            assert!(
                source(name).is_some(),
                "`{name}`'s authored TypeScript is what the sandbox runs"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn every_rule_of_a_shared_component_names_a_different_index() {
        // The specific failure: several rules in one artifact, and a table that gave two of them
        // the same index would dispatch one config entry to the other's handler. Nothing about
        // the output would look wrong — a rule that reports what a different rule should have
        // reported is still a rule reporting.
        //
        // `go-builtins` is the only shipped shared component; the loop stays so a second one
        // is held to the same check the day it arrives.
        for (component_name, expected, decided_by) in [(
            "go-builtins",
            vec![("no-context-in-struct", 0), ("no-package-init", 1)],
            "`go-rules/main.go`'s `ruleset`",
        )] {
            let hosted: Vec<(&str, u32)> = COMPONENT_RULES
                .iter()
                .filter(|(_, host, _)| *host == component_name)
                .map(|(name, _, index)| (*name, *index))
                .collect();
            assert_eq!(
                hosted, expected,
                "`{component_name}`'s slots moved; {decided_by} decides them"
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
        // Both directions, because each one alone leaves an artifact nobody runs or a rule
        // nobody can. A component no rule names is an artifact embedded in the binary and never
        // executed; a rule naming a component this build does not have is a config entry that
        // resolves to nothing.
        for (component_name, _, _) in BUILT_IN_COMPONENTS {
            assert!(
                COMPONENT_RULES
                    .iter()
                    .any(|(_, host, _)| host == component_name),
                "`{component_name}` is embedded and no rule is hosted by it"
            );
        }

        for (name, host, _) in COMPONENT_RULES {
            assert!(
                BUILT_IN_COMPONENTS.iter().any(|(n, _, _)| n == host),
                "`{name}` names the component `{host}`, which this build does not ship"
            );
            assert!(
                names().any(|rule| rule == *name),
                "`{name}` has a component but is not a built-in rule"
            );
        }
    }

    #[test]
    fn every_component_name_fits_the_refusal_message() {
        // The other half of `lanekeep-js`'s `the_refusal_survives_quickjs_beside_a_long_path`,
        // which derives this budget and cannot see these names — that crate sits below this one.
        //
        // What breaks if this fails is not a build: it is that a user who imports a
        // component-backed built-in from a `lanekeep.config.ts` is told they have a problem and
        // not what to do about it, because QuickJS truncated the remedy off the end. The
        // constant's own documentation says what the honest ways to raise it are.
        //
        // **The rule names, not the component names.** `NotAModule` names what the user wrote,
        // and a user writes `lanekeep/no-restricted-imports`; `typescript-builtins` is the
        // artifact behind it and appears in no message. Iterating the wrong table here would
        // budget for a string nobody ever sees, and would have missed by six characters.
        for (name, _, _) in COMPONENT_RULES {
            assert!(
                name.len() <= lanekeep_js::MAX_COMPONENT_NAME,
                "`{name}` is {} characters and the budget is {} — refusing an import of it \
                 would be truncated before it said what to do instead",
                name.len(),
                lanekeep_js::MAX_COMPONENT_NAME,
            );
        }
    }

    #[test]
    fn a_component_is_webassembly_rather_than_a_placeholder() {
        // `include_bytes!` of a stub or a half-written file compiles, and the failure would be a
        // load error inside whichever test ran first. Four bytes are enough to tell them apart.
        for (name, bytes, _) in BUILT_IN_COMPONENTS {
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
        // Still evaluated as TypeScript, and asked by name rather than assumed: this is what
        // distinguishes "not migrated" from "migrated and the table was not updated".
        assert_eq!(component("no-broad-except"), None);
    }

    #[test]
    fn a_declared_component_whose_host_is_missing_is_distinguished_from_unknown() {
        // The broken-table state: a `COMPONENT_RULES` row whose host is absent from
        // `BUILT_IN_COMPONENTS`. `component` answers `None` for it — indistinguishable from a
        // name that is not a component at all — and `is_declared_component` is the lookup that
        // tells the two apart, so the resolver can refuse it as a lanekeep bug rather than
        // report a misspelling or silently serve a stale source.
        let rules = [("no-context-in-struct", "go-builtins", 0u32)];
        let components: [BuiltInComponent; 0] = [];

        assert!(hosted_in(&rules, &components, "no-context-in-struct").is_none());
        assert!(is_declared_component_in(&rules, "no-context-in-struct"));
        assert!(!is_declared_component_in(&rules, "no-such-rule"));
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
            BUILT_IN_RULES.len() + COMPONENT_RULES.len(),
            "every rule in either table must be listed"
        );
    }

    #[test]
    fn every_table_is_kept_in_order() {
        // `names` sorts, so this is about the source rather than about behavior: a table read
        // top to bottom should be the list a reader expects, and an entry appended in the wrong
        // place is invisible once the output is sorted.
        //
        // `COMPONENT_RULES` is ordered by rule name rather than grouped by component, which is
        // the one place this convention costs something: the four rules sharing one artifact do
        // not read as a group. It is worth it because the column a reader arrives with is the
        // name — `every_rule_of_a_shared_component_names_a_different_index` is where the
        // grouping is asserted instead.
        for table in [
            BUILT_IN_RULES.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            BUILT_IN_COMPONENTS
                .iter()
                .map(|(n, _, _)| *n)
                .collect::<Vec<_>>(),
            COMPONENT_RULES
                .iter()
                .map(|(n, _, _)| *n)
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
        // of every case, through the real engine, and `tests/component_rules.rs` reads every
        // shipped component's own `rules()` and compares it to `COMPONENT_RULES`.
        //
        // **Both halves for a rule that has both**, rather than the exact one instead of the
        // weak one. The Rust and Go rules are components with bytes but no source; the TypeScript
        // rules are modules with source but no bytes. A rule with both would be checked both
        // ways, and a component built from a wrong id would pass the source check and fail the
        // bytes check.
        for name in names() {
            let id = format!("lanekeep/{name}");
            if let Some(source) = source(name) {
                assert!(
                    source.contains(&format!("id: '{id}'")),
                    "`{name}` does not declare the id its specifier implies"
                );
            }
            if let Some((bytes, _)) = component(name) {
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
        assert!(
            source("patterns").is_some(),
            "the shared pattern helpers must resolve"
        );
    }

    /// The two copies of `paths.ts` are one file, and this is what keeps them that way.
    ///
    /// `crates/lanekeep-rules/modules/paths.ts` is what the sandbox serves for a rule that
    /// writes `import { resolveImport } from 'lanekeep/paths'`.
    /// `packages/lanekeep/modules/paths.ts` is what a bundler reaches for that same specifier
    /// when a rule is compiled ahead of time into a component. Both have to exist — the
    /// sandbox has no bundler and the bundler has no `include_str!` — and both have to be the
    /// same bytes.
    ///
    /// **The drift would not announce itself.** Nothing fails to build, no rule errors: a rule
    /// would simply resolve `./a` one way under one engine and another way under the other,
    /// and each answer would be individually plausible. That is the failure the whole module
    /// exists to prevent, arriving through the back door.
    ///
    /// `include_bytes!` rather than a read at run time, so the dependency is a build-time one:
    /// deleting the copy fails to compile, naming this test, rather than failing inside
    /// whichever test happened to run first.
    #[test]
    fn paths_ships_byte_for_byte_to_the_authoring_package() {
        const PACKAGED: &[u8] = include_bytes!("../../../packages/lanekeep/modules/paths.ts");

        let embedded = source("paths").unwrap_or_default().as_bytes();
        assert!(!embedded.is_empty(), "the shared path helpers must resolve");

        let at = embedded
            .iter()
            .zip(PACKAGED)
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| embedded.len().min(PACKAGED.len()));
        assert!(
            embedded == PACKAGED,
            "crates/lanekeep-rules/modules/paths.ts and packages/lanekeep/modules/paths.ts \
             have diverged: {} bytes against {}, first differing at byte {at}\n  \
             they are one file served by two engines — carry the change across, rather than \
             leaving a rule to resolve its imports differently depending on which one ran it",
            embedded.len(),
            PACKAGED.len(),
        );
    }

    /// The two copies of `patterns.ts` are one file, on the same terms as the `paths`
    /// copy above: the sandbox serves the embedded one and a bundler reaches the
    /// packaged one, and a rule must resolve its imports the same way under either.
    #[test]
    fn patterns_ships_byte_for_byte_to_the_authoring_package() {
        const PACKAGED: &[u8] = include_bytes!("../../../packages/lanekeep/modules/patterns.ts");

        let embedded = source("patterns").unwrap_or_default().as_bytes();
        assert!(
            !embedded.is_empty(),
            "the shared pattern helpers must resolve"
        );

        let at = embedded
            .iter()
            .zip(PACKAGED)
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| embedded.len().min(PACKAGED.len()));
        assert!(
            embedded == PACKAGED,
            "crates/lanekeep-rules/modules/patterns.ts and packages/lanekeep/modules/patterns.ts \
             have diverged: {} bytes against {}, first differing at byte {at}\n  \
             they are one file served by two engines — carry the change across, rather than \
             leaving a rule to resolve its imports differently depending on which one ran it",
            embedded.len(),
            PACKAGED.len(),
        );
    }

    /// The five files the migration was required not to touch, pinned by digest.
    ///
    /// **A migration that needed one of these edited would have been the wrong migration.** The
    /// claim the whole move rests on is that a rule authored against the QuickJS host API
    /// compiles into a component and behaves identically — and the only evidence for it that
    /// cannot be argued with is that the rules did not change. "We did not edit them" is not
    /// evidence; it is the thing being asserted.
    ///
    /// Hand-written constants, and that is the difference between this and a digest manifest: the
    /// old `typescript-component-digests.txt` answered a different question — whether the
    /// committed artifact was built from the sources beside it — and was rewritten on every
    /// rebuild. A tripwire something re-blesses is not a tripwire. The component is now built
    /// from these sources at build time rather than committed, so the manifest is gone; these
    /// constants move only when a person changes them, and changing one is a decision about the
    /// frozen set rather than a build step.
    ///
    /// Hashed after folding CRLF to LF, on the same terms as
    /// `crates/lanekeep-wasm/tests/fixture_currency.rs`'s `digest`: there is no `.gitattributes`
    /// in this repository, so a Windows checkout with `core.autocrlf` on holds these same five
    /// files under different bytes than Linux or macOS does, and `include_bytes!` below sees
    /// whichever bytes are actually checked out. The constants were recorded from LF bytes, and
    /// folding LF is a no-op, so they are unchanged — only the set of platforms that reproduce
    /// them grows. [`fold`] is duplicated from that other copy rather than imported: it is six
    /// lines, and the original lives in a `tests/` integration crate of a different crate, which
    /// reaching from a unit test here would cost a new dependency edge rather than a shared one.
    #[test]
    fn the_rules_the_migration_moved_are_byte_for_byte_what_they_were() {
        // The four TypeScript built-ins' digests were re-recorded when the self-check rules
        // came onto main: #95 added `language: ['typescript', 'tsx']` to each, which the
        // `rule-declares-language` self-check rule requires. That is a deliberate change to
        // the frozen set — the addition is the point, not a side effect.
        const FROZEN: &[(&str, &str, &[u8])] = &[
            (
                "rules/no-circular-imports.ts",
                "9a55c43c16b5e20d490ddd5ac38ee677975b5ee6f78c1cef34f7c64747cc662c",
                include_bytes!("../rules/no-circular-imports.ts"),
            ),
            (
                "rules/no-default-export.ts",
                "cfe8786c8d28ae53fcec8aa1bea37d27a0acbde867d98b4cec73df5253dd1ce0",
                include_bytes!("../rules/no-default-export.ts"),
            ),
            (
                "rules/no-restricted-imports.ts",
                "18e3ff58132aa9d17850370de34a976299ad2cc4b27bd6f4f0b5db49fd83dd7d",
                include_bytes!("../rules/no-restricted-imports.ts"),
            ),
            (
                "rules/no-unused-exports.ts",
                "dd34f6b1a528559abc58605e2362011e3030b90b0fdfd1d3e0dd2accc1b5c136",
                include_bytes!("../rules/no-unused-exports.ts"),
            ),
            (
                "modules/paths.ts",
                "92e6793d4d5640229a938b38514863691a63f9bd245a015d6bb581bb49cae65c",
                include_bytes!("../modules/paths.ts"),
            ),
        ];

        for (path, expected, bytes) in FROZEN {
            assert_eq!(
                blake3::hash(&fold(bytes)).to_hex().as_str(),
                *expected,
                "`crates/lanekeep-rules/{path}` changed, and it is one of the five files this \
                 migration is only correct if it did not touch\n  \
                 if the change is deliberate, it is a change to the frozen set: say so, and \
                 re-record the digest with the reasoning"
            );
        }
    }

    /// CRLF to LF, leaving a lone carriage return alone.
    ///
    /// Duplicated from `crates/lanekeep-wasm/tests/fixture_currency.rs`'s helper of the same
    /// name — see the doc comment on
    /// [`the_rules_the_migration_moved_are_byte_for_byte_what_they_were`] for why this copy
    /// exists rather than a shared one.
    fn fold(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw.len());
        let mut bytes = raw.iter().peekable();
        while let Some(&byte) = bytes.next() {
            if byte == b'\r' && bytes.peek() == Some(&&b'\n') {
                continue;
            }
            out.push(byte);
        }
        out
    }

    /// Proof that folding is what lets the frozen digests hold on a Windows checkout.
    ///
    /// The migration test above hashes `include_bytes!` output after folding, and the claim
    /// that makes true is that a CRLF checkout of a frozen file folds to the exact same digest
    /// as the LF one it was recorded from — not merely that the two engines agree with each
    /// other. This builds a synthetic CRLF variant of one frozen file and checks the folded hash
    /// against the recorded constant directly, rather than only comparing two foldings to each
    /// other.
    ///
    /// **Built from the folded bytes, not from `include_bytes!`'s raw output, and that is
    /// load-bearing.** `include_bytes!` reflects whatever this checkout actually has — LF on
    /// macOS and Linux, CRLF on Windows with `core.autocrlf` on — so expanding every `\n` to
    /// `\r\n` directly against it is only correct on the platforms where it was already LF. On a
    /// Windows checkout the raw bytes are already `\r\n`, and replacing `\n` with `\r\n` turns
    /// that into `\r\r\n`; `fold` only drops the `\r` immediately before a `\n`, so one `\r`
    /// survives and the result hashes to the *CRLF* digest instead of the LF one — the exact
    /// failure this test exists to catch, reintroduced by the test's own fixture. Folding first
    /// gives canonical LF regardless of platform, and the synthetic CRLF variant is built from
    /// *that* — the same shape as the `path.join` fixture in `AGENTS.md` that normalized the
    /// path it was meant to test: a fixture whose construction depends on the very thing under
    /// test proves nothing on the platform where that thing differs.
    #[test]
    fn a_crlf_checkout_still_matches_the_frozen_digest() {
        const RECORDED: &str = "9a55c43c16b5e20d490ddd5ac38ee677975b5ee6f78c1cef34f7c64747cc662c";
        let canonical = fold(include_bytes!("../rules/no-circular-imports.ts"));

        let mut crlf = Vec::with_capacity(canonical.len());
        for &byte in &canonical {
            if byte == b'\n' {
                crlf.push(b'\r');
            }
            crlf.push(byte);
        }
        // Both sides are already folded to canonical LF before this comparison, so it holds
        // regardless of whether this checkout gave `include_bytes!` LF or CRLF bytes — unlike
        // comparing against the raw bytes, which would be trivially true on a Windows checkout
        // for the same reason the digest above was wrong: `crlf` and the raw CRLF bytes are the
        // same value there, so that comparison would assert nothing on the one platform this
        // test is for.
        assert_ne!(
            canonical, crlf,
            "the fixture must actually contain newlines, or this proves nothing"
        );

        assert_eq!(
            blake3::hash(&fold(&crlf)).to_hex().as_str(),
            RECORDED,
            "a CRLF checkout of a frozen file must fold to the digest recorded from its LF \
             bytes, or the fold above does not actually fix the Windows failure"
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
