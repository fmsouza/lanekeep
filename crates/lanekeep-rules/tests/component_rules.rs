//! The name → (component, index) table, against what the components actually host.
//!
//! `lanekeep_rules::component` is an explicit table rather than a lookup through the component,
//! because resolving a specifier must not require executing guest code — a config load that had
//! to instantiate a 12.4 MiB engine to find out what `lanekeep/no-default-export` means would
//! pay for the whole component to answer a question about a name. The cost of an explicit table
//! is that it can drift from the thing it describes, and the drift is silent: a config entry
//! would be dispatched to a rule that is not the one it named, and a rule reporting is what a
//! working rule looks like.
//!
//! So the table is asserted, in the gate, against every shipped component's own `rules()`. This
//! is the only place that comparison can be made: `lanekeep-rules` has no wasm runtime of its
//! own — it is a table and four `include_bytes!`es — and `lanekeep-wasm`, which does, sits below
//! it and cannot see which rules ship. A dev-dependency is what closes that, and it is a
//! dev-dependency rather than a dependency because nothing in the crate's own code runs a guest.
//!
//! **This is also the first thing in the tree to assert that `typescript-builtins.wasm` hosts
//! four working rules.** The component was committed by the task before this one, whose smoke
//! test was deleted before the commit, so until now nothing but a digest said what was inside
//! it.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::sync::Arc;
use std::time::Duration;

use lanekeep_js::{Limits, RunClock};
use lanekeep_wasm::{ComponentLoader, PermittedImports, WasmEngine, WasmRuntime};

/// Every rule one shipped component enumerates, by id, in index order.
///
/// Through the same loader the engine uses, with the same permitted imports, so a component
/// that would be refused at load is refused here too rather than quietly answering.
fn hosted(name: &str, bytes: &[u8]) -> Vec<String> {
    let engine = WasmEngine::new().expect("the shipped wasmtime configuration builds an engine");
    // A generous per-rule budget and a generous run budget. Compiling a 12.4 MiB component is
    // seconds of work on an idle machine and more under a loaded one, and what is being tested
    // here is a table rather than a limit.
    let limits = Limits::default()
        .with_rule_timeout(Duration::from_mins(2))
        .with_global_timeout(Duration::from_mins(10));
    let clock = RunClock::start(limits.global_timeout);

    let loaded = ComponentLoader::without_cache()
        .permitting(PermittedImports::declared_world())
        .load(&engine, name, bytes)
        .unwrap_or_else(|e| panic!("`{name}` does not load: {e}"));

    let mut runtime = WasmRuntime::new(Arc::clone(&engine), limits, clock)
        .unwrap_or_else(|e| panic!("a runtime for `{name}`: {e}"));
    let instance = runtime
        .instantiate(&loaded)
        .unwrap_or_else(|e| panic!("`{name}` does not instantiate: {e}"));
    runtime
        .call_rules(&instance)
        .unwrap_or_else(|e| panic!("`{name}` cannot list its rules: {e}"))
}

/// One component, its bytes, and every rule the table says it hosts as `(name, index)`.
type Hosted = (&'static str, &'static [u8], Vec<(&'static str, u32)>);

/// Every rule the table claims a component hosts, at the index it claims.
///
/// One instantiation per component rather than one per rule, which is the arrangement the
/// shared component exists for: asking four times would compile the same 12.4 MiB engine four
/// times to read four strings out of one list.
#[test]
fn the_table_agrees_with_every_component_it_names() {
    // The names this build ships, gathered from the table's own public face. Written as the
    // list a reader expects rather than derived from `lanekeep_rules::names()`, because a table
    // checked against itself is not checked: if a rule were dropped from `COMPONENT_RULES`
    // entirely, a derived list would simply stop mentioning it and pass.
    const EXPECTED: &[(&str, u32, &str)] = &[
        ("no-circular-imports", 0, "typescript-builtins"),
        ("no-default-export", 1, "typescript-builtins"),
        ("no-glob-import", 0, "no-glob-import"),
        ("no-restricted-imports", 2, "typescript-builtins"),
        ("no-unused-exports", 3, "typescript-builtins"),
        ("no-unwrap", 0, "no-unwrap"),
    ];

    // A row added to `COMPONENT_RULES` and not to `EXPECTED` would be a rule this test never
    // looks at — and every assertion below would still pass, because they all iterate
    // `EXPECTED`. Counting the shipped component rules through the crate's public face is what
    // forces the new row in here rather than leaving it silently uncovered.
    let shipped = lanekeep_rules::names()
        .filter(|name| lanekeep_rules::component(name).is_some())
        .count();
    assert_eq!(
        EXPECTED.len(),
        shipped,
        "{shipped} rules ship as components and this test names {} of them",
        EXPECTED.len()
    );

    // Grouped by the artifact's bytes, so each component is compiled once.
    let mut by_component: Vec<Hosted> = Vec::new();
    for (rule, index, component) in EXPECTED {
        let (bytes, recorded) =
            lanekeep_rules::component(rule).unwrap_or_else(|| panic!("`{rule}` ships"));
        assert_eq!(
            recorded, *index,
            "`{rule}` is recorded at index {recorded} and this test expects {index}"
        );

        match by_component
            .iter_mut()
            .find(|(name, _, _)| name == component)
        {
            Some((_, _, rules)) => rules.push((rule, *index)),
            None => by_component.push((component, bytes, vec![(rule, *index)])),
        }
    }

    for (component, bytes, rules) in &by_component {
        let ids = hosted(component, bytes);

        for (rule, index) in rules {
            let declared = ids.get(*index as usize).unwrap_or_else(|| {
                panic!(
                    "`{rule}` is recorded at index {index} of `{component}`, which hosts {} \
                     rule(s): {ids:?}",
                    ids.len()
                )
            });
            assert_eq!(
                declared,
                &format!("lanekeep/{rule}"),
                "`lanekeep/{rule}` is recorded at index {index} of `{component}` and that slot \
                 holds `{declared}` — a config naming the first would run the second, and a \
                 rule reporting is what a working rule looks like"
            );
        }

        // And nothing else. A component hosting a rule no name reaches is a rule that cannot be
        // configured, which is invisible from the table's side: every entry would still check
        // out. For the shared component this is also the assertion that the entry module lists
        // exactly the four rules it is supposed to.
        assert_eq!(
            ids.len(),
            rules.len(),
            "`{component}` hosts {} rule(s) and {} are named: {ids:?}",
            ids.len(),
            rules.len()
        );
    }
}

/// The shared component hosts four rules, and they are the four.
///
/// Separate from the test above because it asserts a different thing: that one names the right
/// slot for each rule, this one names the whole enumeration in order. A component that gained a
/// fifth rule, or lost one, passes every per-rule assertion above for the rules it still has.
#[test]
fn the_shared_component_enumerates_the_four_typescript_built_ins() {
    let (bytes, _) =
        lanekeep_rules::component("no-default-export").expect("the shared component ships");
    let ids = hosted("typescript-builtins", bytes);

    assert_eq!(
        ids,
        vec![
            "lanekeep/no-circular-imports",
            "lanekeep/no-default-export",
            "lanekeep/no-restricted-imports",
            "lanekeep/no-unused-exports",
        ],
        "the order is the one `crates/lanekeep-rules/typescript/entry.ts` passes to `register`, \
         and it is what `COMPONENT_RULES` dispatches on"
    );
}

/// The four rules of the shared component are one artifact, not four.
///
/// The whole reason `world rule` takes a rule index. Measured when the component was built: one
/// rule costs 13,021,569 bytes and four cost 13,028,097, so an artifact each would multiply a
/// StarlingMonkey build by the number of rules that ship. If these ever stop being the same
/// bytes, that arrangement has been undone by accident.
#[test]
fn the_four_typescript_built_ins_share_one_artifact() {
    let mut seen: Option<*const u8> = None;
    for rule in [
        "no-circular-imports",
        "no-default-export",
        "no-restricted-imports",
        "no-unused-exports",
    ] {
        let (bytes, _) = lanekeep_rules::component(rule).expect("the rule ships");
        match seen {
            None => seen = Some(bytes.as_ptr()),
            Some(first) => assert!(
                std::ptr::eq(first, bytes.as_ptr()),
                "`{rule}` is served from a different artifact than the rules beside it"
            ),
        }
    }
}
