//! Built-in rules shipped with lanekeep.
//!
//! The rules shipping with lanekeep, authored against the same host API that project-authored
//! rules use and embedded into the binary at build time. Four are TypeScript modules evaluated
//! in QuickJS; the rest are WebAssembly components — two built from `rust-rules/`, and four
//! authored in TypeScript and compiled ahead of time into one shared component.
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
//! guarantee.** Four rules ship as a component *and* keep their TypeScript here, because that
//! source is what the component was built from and what
//! `crates/lanekeep-rules/tests/no_default_export.rs` and its sibling hold to their cases. A
//! resolver that answered from the source first would let `lanekeep.config.ts` run the QuickJS
//! copy while `lanekeep.json` ran the component — one id, two programs, and nothing in the
//! output to say which one reported.
//!
//! Nothing here is written to disk in either case, and a built-in cannot be shadowed by a file
//! in the project.
//!
//! The same prefix also serves shared modules — `lanekeep/paths` and friends — which are
//! not rules and are listed separately for that reason.

/// The rules this build runs as TypeScript modules, as `(name, source)`.
///
/// Evaluated in QuickJS, from source, on every run. What is left here after the four flagship
/// TypeScript rules were compiled ahead of time: three rules targeting Python and one targeting
/// Go, none of which any component hosts yet.
///
/// Ordered, so the source stays greppable and a diff shows what moved. Nothing derives an
/// order from this table directly — see [`names`], which merges the tables and sorts.
const BUILT_IN_RULES: &[(&str, &str)] = &[
    (
        "no-broad-except",
        include_str!("../rules/no-broad-except.ts"),
    ),
    (
        "no-context-in-struct",
        include_str!("../rules/no-context-in-struct.ts"),
    ),
    (
        "no-mutable-default-argument",
        include_str!("../rules/no-mutable-default-argument.ts"),
    ),
    (
        "no-package-init",
        include_str!("../rules/no-package-init.ts"),
    ),
];

/// The TypeScript a component-hosted rule was authored in, as `(name, source)`.
///
/// **Not what runs, and not importable.** Every name here is in [`COMPONENT_RULES`], which is
/// what a config resolves; `just typescript-builtins` is what turns these four files into
/// `components/typescript-builtins.wasm`. They are kept because a rule's source is the thing
/// a reader reviews and the thing `crates/lanekeep-rules/tests/no_default_export.rs` and
/// `.../no_restricted_imports.rs` hold to their cases — those two run the author's TypeScript
/// through QuickJS, which is the engine it was written against and no longer the engine it
/// ships on.
///
/// The two claims that keeps honest are different and both are needed. That the *source* is
/// right is what those tests assert. That the *artifact* is this source compiled is what
/// `crates/lanekeep-wasm/tests/fixture_currency.rs` asserts, against
/// `tests/typescript-component-digests.txt` — `componentize-js` is not byte-reproducible, so a
/// digest of the inputs is the only staleness check available. Neither substitutes for the
/// other: source tests over a stale artifact pass while the shipped rule is a previous version,
/// and a current artifact built from a wrong rule is current and wrong.
///
/// Ordered, on the same terms as [`BUILT_IN_RULES`].
const COMPONENT_SOURCES: &[(&str, &str)] = &[
    (
        "no-circular-imports",
        include_str!("../rules/no-circular-imports.ts"),
    ),
    (
        "no-default-export",
        include_str!("../rules/no-default-export.ts"),
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

/// The components this build ships, as `(component name, bytes)`.
///
/// **Keyed by the component rather than by a rule**, because a component hosts one rule or
/// several and the artifact is what is embedded. `typescript-builtins` hosts four; the two
/// built from `rust-rules/<name>/` host one each and are named after it.
///
/// A rule is authored once and runs once — [`COMPONENT_RULES`] and [`BUILT_IN_RULES`] are
/// disjoint, and a name in both would be two programs answering to one id with nothing to say
/// which one ran. [`COMPONENT_SOURCES`] is not a third answer to that question: it is the
/// authored text of a rule this table already hosts, and nothing resolves a specifier through
/// it.
///
/// Built by `just rust-rules` and `just typescript-builtins`, which are also what copy the
/// artifacts here. The bytes are committed, so the gate needs neither `cargo component`, a wasm
/// target, nor Node.
///
/// One shipped component: its name, its bytes, and its source map if it has one.
///
/// Named so the two tables below and the lookup behind them read as rows rather than as tuples
/// — and because a three-element tuple of two slices and an optional slice is a shape clippy is
/// right to object to.
type BuiltInComponent = (&'static str, &'static [u8], Option<&'static [u8]>);

/// # The third column is the component's source map, and it is optional
///
/// A component compiled from TypeScript throws from a position in the bundle several files were
/// flattened into — `matches@entry.js:957:16` — and `entry.js` is a file nobody can open. The
/// sidecar map beside the artifact is what turns that back into a line in the source a reader can
/// go and look at, and it is embedded here for the reason the bytes are: a built-in is in the
/// binary, so nothing is read from disk and nothing in a project can shadow it.
///
/// `None` for the two components built from Rust, and it is not an omission: a Rust rule that
/// fails does so by panicking, which traps, and a trap arrives at the host with no stack at all.
/// There would be nothing to remap.
///
/// **In this table rather than beside it**, so that a component and its map cannot be wired
/// separately. They are one build's output — `just typescript-builtins` writes both — and a map
/// paired with any other component reports arbitrary lines of real files, which is worse than
/// reporting none.
///
/// Ordered, on the same terms as [`BUILT_IN_RULES`].
const BUILT_IN_COMPONENTS: &[BuiltInComponent] = &[
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
    (
        "typescript-builtins",
        include_bytes!("../components/typescript-builtins.wasm"),
        Some(include_bytes!("../components/typescript-builtins.wasm.map")),
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
/// `register`. Inserting a rule in the middle of that array renumbers every rule after it, so
/// a new one goes wherever the alphabet puts it and this table is re-recorded.
///
/// Ordered by rule name, on the same terms as [`BUILT_IN_RULES`] — a reader looking a rule up
/// is looking up a name, and the component column is what they learn.
const COMPONENT_RULES: &[(&str, &str, u32)] = &[
    ("no-circular-imports", "typescript-builtins", 0),
    ("no-default-export", "typescript-builtins", 1),
    ("no-glob-import", "no-glob-import", 0),
    ("no-restricted-imports", "typescript-builtins", 2),
    ("no-unused-exports", "typescript-builtins", 3),
    ("no-unwrap", "no-unwrap", 0),
];

/// Shared modules the built-in rules import, and project rules may too.
///
/// Separate from the rules because they are not rules: they have no id, no card and no
/// query, so anything that lists rules must not list them. They resolve through the same
/// `lanekeep/` prefix, which is deliberate — a helper two built-ins both need is a helper a
/// project rule will need eventually, and there is nothing to gain from hiding it.
const BUILT_IN_MODULES: &[(&str, &str)] = &[("paths", include_str!("../modules/paths.ts"))];

/// The TypeScript behind a `lanekeep/<name>`, rule or shared module.
///
/// Returns `None` for anything else, so an unknown built-in surfaces as an unresolved
/// import naming the specifier rather than as a rule that silently never runs.
///
/// **Answering here does not make a name importable.** The sources a component was compiled
/// from are in the chain, so the four rules inside `typescript-builtins` answer with the text
/// they were authored in — which is what the tests that run them through QuickJS need, and is
/// not what a config resolves. `lanekeep_js::RuleRoot::resolve` asks [`component`] first and
/// refuses a name it answers, so this lookup is never reached for one.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    BUILT_IN_RULES
        .iter()
        .chain(COMPONENT_SOURCES)
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
/// whichever rule the component happens to enumerate first, and for three of the four
/// TypeScript built-ins that is a different rule than the one the config named.
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

/// The row of [`BUILT_IN_COMPONENTS`] that hosts a rule, and the index it sits at.
///
/// One lookup behind both accessors, so a rule cannot be found by one and missed by the other.
fn hosted(name: &str) -> Option<(&'static BuiltInComponent, u32)> {
    let (_, host, index) = COMPONENT_RULES
        .iter()
        .find(|(candidate, _, _)| *candidate == name)?;
    let component = BUILT_IN_COMPONENTS
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
        // `COMPONENT_RULES`, not `source` against `component`: four rules answer `source` with
        // the TypeScript their component was compiled from, and answering is not resolving.
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
    fn a_name_is_in_exactly_one_of_the_three_tables() {
        // The third table is `COMPONENT_SOURCES`, and it is the one that can go wrong quietly.
        // A name there and in `BUILT_IN_RULES` would be a rule whose source is served to the
        // sandbox *and* compiled into a component; a name there and in no component would be
        // TypeScript nothing runs, sitting in the binary looking exactly like a rule that ships.
        for (name, _) in COMPONENT_SOURCES {
            assert!(
                component(name).is_some(),
                "`{name}` has a source recorded as a component's original and no component \
                 hosts it — it is text in the binary that nothing runs"
            );
            assert!(
                !BUILT_IN_RULES.iter().any(|(n, _)| n == name),
                "`{name}` is both a module this build evaluates and the original of a component"
            );
        }

        // And the pairing is one-to-one in the direction that matters for dispatch: no two
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
    fn the_migrated_rules_are_components_and_not_modules() {
        // The swap itself, named. The generic tests above hold whichever table a rule is in,
        // which is what makes them survive a migration — and is also what would let any of
        // these quietly revert to TypeScript with nothing red.
        //
        // Two shapes, deliberately together. The Rust pair has no source at all; the TypeScript
        // four keep theirs and must still not be *served* as modules, which is the weaker and
        // more easily lost half.
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

        for name in [
            "no-circular-imports",
            "no-default-export",
            "no-restricted-imports",
            "no-unused-exports",
        ] {
            let (bytes, _) = component(name).unwrap_or_default();
            assert!(
                !bytes.is_empty(),
                "`{name}` ships as a component and does not"
            );
            assert!(
                !BUILT_IN_RULES.iter().any(|(n, _)| *n == name),
                "`{name}` is back in the table the sandbox evaluates from"
            );
            assert!(
                source(name).is_some(),
                "`{name}`'s authored TypeScript is what its component was built from and what \
                 its tests run — it is kept, and only stops being importable"
            );
        }
    }

    #[test]
    fn every_rule_of_a_shared_component_names_a_different_index() {
        // The specific failure: four rules in one artifact, and a table that gave two of them
        // the same index would dispatch one config entry to the other's handler. Nothing about
        // the output would look wrong — a rule that reports what a different rule should have
        // reported is still a rule reporting.
        let hosted: Vec<(&str, u32)> = COMPONENT_RULES
            .iter()
            .filter(|(_, host, _)| *host == "typescript-builtins")
            .map(|(name, _, index)| (*name, *index))
            .collect();
        assert_eq!(
            hosted,
            vec![
                ("no-circular-imports", 0),
                ("no-default-export", 1),
                ("no-restricted-imports", 2),
                ("no-unused-exports", 3),
            ],
            "the shared component's slots moved; `typescript/entry.ts` decides them"
        );
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
        // nobody can. A component no rule names is 13 MiB embedded in the binary and never
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
            COMPONENT_SOURCES
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>(),
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
        // weak one. The four TypeScript rules in a component have a source *and* bytes, and the
        // two claims are about different things: the source says what was authored, the bytes
        // say what was compiled. Checking only the source would pass against an artifact built
        // from a previous id.
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

    /// The five files the migration was required not to touch, pinned by digest.
    ///
    /// **A migration that needed one of these edited would have been the wrong migration.** The
    /// claim the whole move rests on is that a rule authored against the QuickJS host API
    /// compiles into a component and behaves identically — and the only evidence for it that
    /// cannot be argued with is that the rules did not change. "We did not edit them" is not
    /// evidence; it is the thing being asserted.
    ///
    /// Hand-written constants, and that is the difference between this and
    /// `crates/lanekeep-wasm/tests/typescript-component-digests.txt`. That manifest answers a
    /// different question — whether the committed artifact was built from the sources beside it
    /// — and `just typescript-builtins` rewrites it, correctly, on every rebuild. A tripwire
    /// something re-blesses is not a tripwire. These move only when a person changes them, and
    /// changing one is a decision about the frozen set rather than a build step.
    #[test]
    fn the_rules_the_migration_moved_are_byte_for_byte_what_they_were() {
        const FROZEN: &[(&str, &str, &[u8])] = &[
            (
                "rules/no-circular-imports.ts",
                "3ba0f2ba906a2b900740865d67cdb0f9c307d179916f8da6d2755f1aea532c1a",
                include_bytes!("../rules/no-circular-imports.ts"),
            ),
            (
                "rules/no-default-export.ts",
                "ecbf4b4c78b1a6142a44121d435a4aeefc841685692c50673ac64b9a741c8b54",
                include_bytes!("../rules/no-default-export.ts"),
            ),
            (
                "rules/no-restricted-imports.ts",
                "f879809c5c944428568f3c64665e1ed8ed6c03d25cbea5c3a9dc0f833e1d4ad4",
                include_bytes!("../rules/no-restricted-imports.ts"),
            ),
            (
                "rules/no-unused-exports.ts",
                "0ab09def28deb0a83e121c8d0d7fbdd63a865efe5677ed1229e393d20ddac2b2",
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
                blake3::hash(bytes).to_hex().as_str(),
                *expected,
                "`crates/lanekeep-rules/{path}` changed, and it is one of the five files this \
                 migration is only correct if it did not touch\n  \
                 if the change is deliberate, it is a change to the frozen set: say so, and \
                 re-record the digest with the reasoning"
            );
        }
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
