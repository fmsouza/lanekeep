//! `check-context`'s navigation and reporting, driven by a real component.
//!
//! Unlike `tests/world_shape.rs`, the host here is the host: `lanekeep_wasm::host` over a
//! real `NodeArena` built from a real parse. What is under test is that a rule running as a
//! WebAssembly component sees the same tree, at the same positions, as the identical rule
//! running under QuickJS — every assertion below is the one
//! `crates/lanekeep-js/src/host.rs`'s test of the same name makes, restated against the
//! component boundary.
//!
//! That correspondence is the point rather than a convenience. Two engines that disagreed
//! about what `parent` or `text` means for one file would let a single run disagree with
//! itself, which is what sharing one `NodeArena` between them exists to prevent — and a
//! shared type only removes the question if both callers actually agree about how to call it.
//!
//! # The guest is a probe and the assertions live here
//!
//! `tests/fixtures/navigation/` walks the tree and encodes what it observed into each
//! report's message; this file asserts on the recorded reports. So each assertion below
//! covers two things at once: the message, which is what the guest saw through the boundary,
//! and the line and column, which the host derived independently from the node the guest
//! reported at.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` is listed because only it fires: nothing here panics
// directly, and an unfulfilled `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, HostState, Report};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const NAVIGATION: &[u8] = include_bytes!("fixtures/navigation.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

/// The two-statement source most probes run against.
///
/// The same string `lanekeep-js`'s `reads_text_and_position` uses, so the line and column
/// numbers asserted here are comparable to that test's by inspection.
const TWO_STATEMENTS: &str = "const x = 1;\nconst y = 2;";

/// Parse a source, lend a context over it to the fixture, and call one probe.
///
/// Returns the store, the context handle and the call's own outcome rather than the reports,
/// so a test that needs to look at the context twice — or at a call that trapped — can.
fn run(
    source: &str,
    name: &str,
) -> (
    Store<HostState>,
    Resource<CheckContext>,
    wasmtime::Result<()>,
) {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, NAVIGATION).expect("the fixture is a valid component");
    let mut linker = Linker::new(&engine);
    Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .expect("the real host satisfies every import the world declares");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parses");

    let mut store = Store::new(&engine, HostState::new());
    let context = store
        .data_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            FILE,
        ))
        .expect("the resource table accepts a context");

    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");
    let captures = vec![types::MatchEntry {
        name: name.to_owned(),
        node: NodeArena::ROOT,
    }];
    let outcome = rule.call_check(&mut store, Resource::new_borrow(context.rep()), &captures);

    (store, context, outcome)
}

/// Everything the context has recorded since it was last asked.
fn take(store: &mut Store<HostState>, context: &Resource<CheckContext>) -> Vec<Report> {
    store
        .data_mut()
        .check_context_mut(context)
        .expect("the context outlives the call that borrowed it")
        .take_reports()
}

/// Run one probe and collect what it reported.
fn probe(source: &str, name: &str) -> Vec<Report> {
    let (mut store, context, outcome) = run(source, name);
    outcome.expect("check returns without trapping");
    take(&mut store, &context)
}

/// Render reports as `line:column message`, so an assertion reads as what a reader would see
/// and a failure names the position rather than dumping a struct.
fn rendered(reports: &[Report]) -> Vec<String> {
    reports
        .iter()
        .map(|report| {
            format!(
                "{}:{} {}",
                report.line,
                report.column,
                report.message.as_deref().unwrap_or("<no message>")
            )
        })
        .collect()
}

#[test]
fn exposes_the_file_path_and_text() {
    assert_eq!(
        rendered(&probe("const x = 1;", "file")),
        ["1:1 src/example.ts | const x = 1;"],
        "the guest read both whole-file accessors through the borrowed context"
    );
}

#[test]
fn navigates_from_the_root() {
    assert_eq!(
        rendered(&probe(TWO_STATEMENTS, "navigate")),
        [
            "1:1 program",
            "1:1 2 named children",
            "1:1 lexical_declaration",
            "2:1 const y = 2;",
            // The load-bearing line. `lanekeep-js`'s `reads_text_and_position` asserts 2 and
            // 1 for this node, and so does `lanekeep-query` when it builds a location, so a
            // zero-based reading here would put every component rule's violations one line
            // and one column off what the identical TypeScript rule reports.
            "2:1 line=Some(2) column=Some(1)",
        ]
    );
}

#[test]
fn walks_up_and_back_down() {
    assert_eq!(
        rendered(&probe("const x = 1;", "walk")),
        [
            "1:1 parent-of-inner-is-declaration=true parent-of-declaration-is-root=true \
             root-has-parent=false same-node-same-handle=true",
            // The root's handle is zero, and `parent` returning `option<node>` is the whole
            // reason: a host that collapsed absence and zero together would answer `None`
            // here, and every top-level item in every file would look parentless.
            "1:1 root=0 parent-of-declaration=Some(0)",
        ]
    );
}

#[test]
fn ancestors_end_at_the_root() {
    assert_eq!(
        rendered(&probe("function f() { return 1; }", "ancestors")),
        [
            "1:16 len=3 first-is-body=true last-is-root=true contains-function=true \
             root-ancestors=0"
        ]
    );
}

#[test]
fn named_children_omits_anonymous_tokens() {
    assert_eq!(
        rendered(&probe("const x = 1;", "named")),
        ["1:1 all=3 named=1 first-token-is-named=false every-named-child-is-named=true"]
    );
}

#[test]
fn an_unresolvable_handle_returns_nothing_rather_than_trapping() {
    // Rule code is arbitrary and will pass stale or invented numbers. Trapping here would
    // abort the run over a mistyped variable in a rule.
    let reports = probe("const x = 1;", "unresolvable");

    assert_eq!(
        rendered(&reports),
        [
            "1:1 kind=None text=None line=None column=None parent=None is-named=false \
             children=0 named-children=0 ancestors=0 loc=false"
        ]
    );
    assert_eq!(
        reports.len(),
        1,
        "the guest also reported *at* the unresolvable handle, and that one must be dropped \
         rather than recorded at a position a reader would go and look at"
    );
}

#[test]
fn loc_carries_the_file_and_the_same_position_a_report_would() {
    assert_eq!(
        rendered(&probe(TWO_STATEMENTS, "loc")),
        ["2:1 src/example.ts:2:1"]
    );
}

#[test]
fn a_fix_is_a_byte_range_taken_from_the_node_it_names() {
    // A rule already has the node it matched; offsets it computed itself are offsets it can
    // get wrong, which is the one mistake that would let a fix corrupt a file.
    let reports = probe(TWO_STATEMENTS, "fix");

    assert_eq!(
        rendered(&reports),
        [
            "2:1 replace it",
            "1:1 a suggestion",
            "1:1 a fix at a handle that does not resolve",
        ]
    );

    let fixes: Vec<Option<(usize, usize, &str, bool)>> = reports
        .iter()
        .map(|report| {
            report
                .fix
                .as_ref()
                .map(|fix| (fix.start, fix.end, fix.replacement.as_str(), fix.safe))
        })
        .collect();
    assert_eq!(
        fixes,
        [
            Some((13, 25, "const y = 3;", true)),
            Some((0, 12, "const x = 2;", false)),
            // Dropped rather than guessed at: a fix at the wrong offsets rewrites the wrong
            // code. The report it came with survives, because what the rule found is still
            // true.
            None,
        ]
    );
}

#[test]
fn a_fix_that_does_not_say_whether_it_is_safe_is_a_suggestion() {
    // The reason `fix.safe` is `option<bool>` rather than `bool`. `--fix` applies the safe
    // ones, so the two mistakes are not symmetric: a suggestion that should have been safe
    // costs a manual edit, and a fix that should have been a suggestion rewrites someone's
    // code. A `bool` cannot carry "did not say", so every authoring crate would have to pick a
    // value on its author's behalf, and one picking `true` would make fixes auto-applicable
    // with nothing here able to notice. This asserts the host owns that answer.
    let reports = probe("const x = 1;", "unsaid-fix");
    let fix = reports
        .first()
        .and_then(|report| report.fix.as_ref())
        .expect("the fix survives, since the node it names resolves");

    assert!(
        !fix.safe,
        "a fix that did not say must not be auto-applicable"
    );
    assert_eq!(
        (fix.start, fix.end, fix.replacement.as_str()),
        (0, 12, "const x = 3;"),
        "and the rest of the fix is unaffected by the field being absent"
    );
}

#[test]
fn the_no_message_form_records_a_report_with_no_message() {
    // `report`'s two `option` parameters are independent, which is the shape the TypeScript
    // API could only express as a union second argument.
    let reports = probe("const x = 1;", "bare");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].message, None);
    assert_eq!((reports[0].line, reports[0].column), (1, 1));
    assert_eq!(reports[0].fix, None);
}

#[test]
fn taking_reports_empties_the_context() {
    // Mirrors `lanekeep-js`'s test of the same name. Taken twice would be reported twice.
    let (mut store, context, outcome) = run("const x = 1;", "bare");
    outcome.expect("check returns");
    assert_eq!(take(&mut store, &context).len(), 1);
    assert!(
        take(&mut store, &context).is_empty(),
        "reports must not be reported twice"
    );
}

#[test]
fn a_declared_but_unimplemented_method_traps_rather_than_answering() {
    // The decision this makes checkable: every value an unimplemented method could return
    // instead — `none` for a date, `false` for a shadow, an empty list for a query — reads as
    // a real answer, and a rule built against one would look like it was working while the
    // run was quietly wrong. When a later change implements `today`, this test changes with
    // it, which is the point.
    let (mut store, context, outcome) = run("const x = 1;", "unimplemented");

    let trap = outcome.expect_err("the call traps");

    // The **root cause**, not the whole `{:?}` render, and the difference is not cosmetic.
    // wasmtime prefixes a host error with a wasm backtrace whose top frame is spelled
    // `wit-component:shim!indirect-lanekeep:host/types@0.1.0-[method]check-context.today` —
    // so an assertion over the rendered error contains the method name whatever the host
    // said, and holds even against a host that named a different method entirely. Measured:
    // that version of this test survived a mutation replacing the name with `something`.
    let cause = trap.root_cause().to_string();
    assert!(
        cause.contains("check-context.today"),
        "the trap names the method a reader has to go and look at: {cause}"
    );
    assert!(
        cause.contains("not implemented yet"),
        "and says what is missing rather than only that something is: {cause}"
    );

    assert_eq!(
        rendered(&take(&mut store, &context)),
        ["1:1 recorded before the trap"],
        "a handler may report several times and then hit something it cannot do. What it \
         already found is still true, and discarding it would make the failure harder to \
         diagnose rather than easier — the same posture as `lanekeep-js`'s \
         `a_rule_that_throws_still_leaves_earlier_reports`"
    );
}
