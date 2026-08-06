//! `reduce-context`, driven by a real component — and the phase split it exists to enforce.
//!
//! The host here is the host: `lanekeep_wasm::host` over a real [`ReduceContext`]. What is
//! under test is the cross-file phase's whole surface — the file list, the facts, and the one
//! report form that has no node to point at — plus the property that makes the two phases worth
//! separating at all.
//!
//! # The invariant, and the three layers that hold it
//!
//! **The reduce phase never touches parse trees.** Cross-file rules consume facts and the file
//! list, nothing else, because facts are small and serializable and that is what keeps
//! cross-file rules parallel and cacheable. `check-context` and `reduce-context` are two
//! resources with disjoint method sets for exactly that reason: a rule cannot call across the
//! split, because the method is not on the type it holds.
//!
//! Three layers hold it, and each is asserted where it can actually be asserted.
//!
//! 1. **The world declares the sets disjoint.** `tests/facts.rs`'s
//!    `the_world_puts_emit_fact_on_the_check_context_and_nowhere_else` reads `wit/world.wit`
//!    itself and asserts that the two `resource` blocks share exactly one method name. That is
//!    the declaration, and it is where the declaration lives.
//! 2. **A wrong-phase call does not compile.** Confirmed by hand rather than automated, because
//!    a test cannot assert a compile error in a crate built by a different toolchain for a
//!    different target. Measured 2026-08-06, `cargo-component` 0.21.1, `rustc` 1.95.0: adding
//!    `ctx.files()` to this fixture's `check` fails with ``error[E0599]: no method named `files`
//!    found for reference `&types::CheckContext` in the current scope``, and `cargo` exits
//!    non-zero. The deliberate mistake was then removed.
//! 3. **A forged handle traps in the runtime, and this one is automated.** It is the
//!    load-bearing layer: a compile error only guards a rule someone compiles from source and
//!    says nothing about a component that arrives prebuilt. See
//!    [`a_forged_reduce_handle_traps_before_the_host_is_reached`].
//!
//! **What is not claimed is that the other phase is unnameable, because it is not.** Both
//! contexts live in one imported interface, so the guest's bindings contain both types and both
//! method sets and hand out a `pub unsafe fn from_handle(handle: u32)` for each. A per-file rule
//! can name `reduce-context`; what it cannot do is obtain a valid one. Literal unnameability
//! would want one interface per phase, and `wit/world.wit` carries the measurement that rejected
//! that: the split costs three imports where one will do and buys nothing, because the `rule`
//! world exports both passes and so must import both types regardless.
//!
//! # Assertions on a trap read `root_cause`
//!
//! wasmtime prefixes a host function's error with a backtrace whose top frame already spells the
//! method name, so `format!("{error:?}").contains("reduce-context.report")` passes whatever the
//! host said. `root_cause()` is the host's own message and nothing else.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]` modules
// and nothing else, so the helpers below — which are neither — need the grant restating. Only
// `expect_used` fires: nothing out here panics directly, and an unfulfilled `expect` attribute
// is itself an error, so a speculative second lint would fail the gate.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types::EmittedFact;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, Fact, HostState, ReduceContext, ReduceReport};
use wasmtime::Store;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const REDUCE: &[u8] = include_bytes!("fixtures/reduce.wasm");

/// The path the per-file context reports as the file under check.
///
/// Only the forgery probes ever enter `check`, and none of them reads this. A context has to
/// have parsed something all the same.
const FILE: &str = "src/example.ts";

/// The file under check. Nothing here queries it.
const SOURCE: &str = "const alpha = 1;\n";

/// One `emitted-fact`, as the engine will build one: kind, the file it came from, and the
/// payload exactly as the guest serialized it.
///
/// Three fields rather than two, and that is the shape difference from `lanekeep-js` written
/// down as a constructor: its `ReduceFact` carries `kind` and a `json` with the file already
/// *spliced into the payload* by `merge_file`, because a JavaScript reduce phase has nowhere
/// else to read a file from. The world has somewhere else, so nothing is spliced.
fn fact(kind: &str, file: &str, data: &str) -> EmittedFact {
    EmittedFact {
        kind: kind.to_owned(),
        file: file.to_owned(),
        data: data.to_owned(),
    }
}

/// One instantiated component, one store, and both phases' contexts lent from it.
///
/// Both, because the forgery probes need the *other* phase's context to exist and be readable
/// afterwards: the assertion that matters is not only that the call trapped but that the other
/// phase's host state recorded nothing.
struct Harness {
    store: Store<HostState>,
    rule: Rule,
    reduce: Resource<ReduceContext>,
    check: Resource<CheckContext>,
}

impl Harness {
    /// Build a harness over a file list and a fact list.
    ///
    /// `files[0]` is the probe the guest runs and the rest are its arguments — the fixture's own
    /// convention, which it documents. `reduce` is handed nothing but a context, so a
    /// host-chosen list of strings has nowhere else to arrive.
    fn new(files: &[&str], facts: Vec<EmittedFact>) -> Self {
        let engine = engine().expect("the shipped wasmtime configuration builds an engine");
        let component = Component::new(&engine, REDUCE).expect("the fixture is a valid component");
        let mut linker = Linker::new(&engine);
        Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .expect("the real host satisfies every import the world declares");

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(SOURCE, None).expect("parses");

        let mut store = Store::new(&engine, HostState::new());
        let reduce = store
            .data_mut()
            .push_reduce_context(ReduceContext::new(
                files.iter().map(|file| (*file).to_owned()).collect(),
                facts,
            ))
            .expect("the resource table accepts a context");
        let check = store
            .data_mut()
            .push_check_context(CheckContext::new(
                NodeArena::new(tree, SOURCE.to_owned()),
                FILE,
                Arc::new(TypeScript),
            ))
            .expect("the resource table accepts a context");
        let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

        Self {
            store,
            rule,
            reduce,
            check,
        }
    }

    /// Run the cross-file pass. `Err` is a trap.
    fn reduce(&mut self) -> wasmtime::Result<()> {
        self.rule
            .call_reduce(&mut self.store, Resource::new_borrow(self.reduce.rep()))
    }

    /// Run the per-file pass with one capture naming the probe. `Err` is a trap.
    fn check(&mut self, probe: &str) -> wasmtime::Result<()> {
        let captures = vec![types::MatchEntry {
            name: probe.to_owned(),
            node: NodeArena::ROOT,
        }];
        self.rule.call_check(
            &mut self.store,
            Resource::new_borrow(self.check.rep()),
            &captures,
        )
    }

    /// What the cross-file context would hand the engine, emptying it as the engine's call does.
    fn reports(&mut self) -> Vec<ReduceReport> {
        self.store
            .data_mut()
            .reduce_context_mut(&self.reduce)
            .expect("the context outlives the call that borrowed it")
            .take_reports()
    }

    /// The cross-file reports' messages, for the probes that encode an observation into one.
    fn messages(&mut self) -> Vec<String> {
        self.reports()
            .into_iter()
            .map(|report| report.message.unwrap_or_else(|| "<no message>".to_owned()))
            .collect()
    }

    /// The per-file reports' messages, on the same terms.
    fn check_messages(&mut self) -> Vec<String> {
        self.store
            .data_mut()
            .check_context_mut(&self.check)
            .expect("the context outlives the call that borrowed it")
            .take_reports()
            .into_iter()
            .map(|report| report.message.unwrap_or_else(|| "<no message>".to_owned()))
            .collect()
    }

    /// What the per-file context recorded for the reduce phase.
    fn check_facts(&mut self) -> Vec<Fact> {
        self.store
            .data_mut()
            .check_context_mut(&self.check)
            .expect("the context outlives the call that borrowed it")
            .take_facts()
    }
}

/// Three facts of two kinds, in an order no sort would produce.
///
/// Deliberately not alphabetical by kind and not alphabetical by file: a host that ordered
/// either would be caught by [`the_facts_keep_the_order_the_engine_supplied`], and the same
/// fixture is what the counting tests select over.
fn corpus() -> Vec<EmittedFact> {
    vec![
        fact("export", "src/z.ts", r#"{"n":1}"#),
        fact("import", "src/a.ts", r#"{"n":2}"#),
        fact("export", "src/m.ts", r#"{"n":3}"#),
    ]
}

#[test]
fn both_passes_are_declared() {
    // The fixture is the first here with a real cross-file pass, so the probe that discovers it
    // is worth asserting on this artifact rather than only on `world-shape.wasm`.
    let mut harness = Harness::new(&["files"], Vec::new());
    assert!(
        harness
            .rule
            .call_has_check(&mut harness.store)
            .expect("has-check returns")
    );
    assert!(
        harness
            .rule
            .call_has_reduce(&mut harness.store)
            .expect("has-reduce returns")
    );
}

#[test]
fn the_file_list_crosses_unchanged() {
    // `the_file_list_is_visible` under QuickJS, plus the half that test could not assert: the
    // order. These are out of alphabetical order on purpose — the engine's list is already
    // deterministic, so a sort at this boundary would be a second ordering, and two orderings
    // that ever disagreed would let a rule that stops at the first match see a different corpus
    // depending on which one ran last.
    let mut harness = Harness::new(&["files", "src/z.ts", "src/a.ts", "src/m.ts"], Vec::new());
    harness.reduce().expect("reduce returns");

    assert_eq!(
        harness.messages(),
        ["files=files,src/z.ts,src/a.ts,src/m.ts"]
    );
}

#[test]
fn a_fact_arrives_as_record_fields() {
    // `facts_come_back_as_objects`, translated: under QuickJS a fact arrives as a parsed
    // JavaScript object and the test reads a property off it; here it arrives as an
    // `emitted-fact` record and the guest reads three fields. Reported separately rather than as
    // one rendering, because a host that filled the wrong field would still produce a plausible
    // line.
    let mut harness = Harness::new(
        &["fact-fields"],
        vec![fact("export", "src/a.ts", r#"{"symbol":"parse"}"#)],
    );
    harness.reduce().expect("reduce returns");

    assert_eq!(
        harness.messages(),
        [r#"export|src/a.ts|{"symbol":"parse"}"#]
    );
}

#[test]
fn the_file_travels_beside_the_payload_and_never_inside_it() {
    // The one place the two engines hand a reduce rule genuinely different bytes, asserted
    // rather than only described.
    //
    // `lanekeep-js` splices the file into the payload with `merge_file`, at reduce time, because
    // a JavaScript rule reads `f.file` off a parsed object and there is nowhere else to put it —
    // and that splice *overrides* a `"file"` the rule wrote itself, so a rule cannot attribute
    // its own fact to a file it did not come from. The world has somewhere else to put it, so
    // `emitted-fact.file` is the host's field and `data` is untouched.
    //
    // The protection is the same and the mechanism differs: the rule's own `"file"` survives,
    // because inside `data` it is inert — nothing reads `data` looking for an attribution. What
    // must not appear is a second `"file"` key, which is what merging at emit time would produce
    // once the engine's reduce-time merge ran over the same bytes.
    let mut harness = Harness::new(
        &["fact-fields"],
        vec![fact("export", "src/truth.ts", r#"{"file":"lies.ts"}"#)],
    );
    harness.reduce().expect("reduce returns");

    let messages = harness.messages();
    assert_eq!(messages, [r#"export|src/truth.ts|{"file":"lies.ts"}"#]);
    assert_eq!(
        messages[0].matches(r#""file""#).count(),
        1,
        "one `file` key, in the payload the rule wrote, and no second one merged in: {messages:?}"
    );
}

#[test]
fn facts_filter_by_kind_and_default_to_everything() {
    // The kinds repeat and are interleaved on purpose: a host that grouped by kind, or that
    // stopped at the first match, would still answer `1` and `1` and pass a laxer assertion.
    let mut harness = Harness::new(&["counts", "export", "import"], corpus());
    harness.reduce().expect("reduce returns");

    assert_eq!(harness.messages(), ["export=2", "import=1", "all=3"]);
}

#[test]
fn an_unknown_kind_selects_nothing_rather_than_everything() {
    // `an_unknown_kind_yields_an_empty_array_rather_than_undefined`, translated: there is no
    // `undefined` in WIT, so what the empty list has to be distinguished from is the *full* one.
    // The `all=3` beside it is what makes this discriminating — a host that ignored `kind`
    // entirely would answer `nope=3`, and a test asserting only `nope=0` against an empty corpus
    // would pass for it.
    let mut harness = Harness::new(&["counts", "nope"], corpus());
    harness.reduce().expect("reduce returns");

    assert_eq!(harness.messages(), ["nope=0", "all=3"]);
}

#[test]
fn the_facts_keep_the_order_the_engine_supplied() {
    // Not sorted here, for the reason the file list is not sorted: the engine has already
    // ordered them by `(rule_id, file, sequence)` through `lanekeep_core::fact::sort`, and a
    // second ordering at this boundary could disagree with the first. Neither the kinds nor the
    // files are in order, so a host that sorted on either would be caught.
    let mut harness = Harness::new(&["fact-fields"], corpus());
    harness.reduce().expect("reduce returns");

    assert_eq!(
        harness.messages(),
        [
            r#"export|src/z.ts|{"n":1}"#,
            r#"import|src/a.ts|{"n":2}"#,
            r#"export|src/m.ts|{"n":3}"#,
        ]
    );
}

#[test]
fn a_report_names_a_file_of_its_own() {
    // `reporting_names_a_file_of_its_own`. A cross-file rule reports at the site a fact came
    // from, which is by definition not "the file being checked" — there is not one. The context
    // holds `src/example.ts` as the per-file path and this report names something else.
    let mut harness = Harness::new(
        &["report", "src/b.ts", "4", "2", "unused export"],
        Vec::new(),
    );
    harness.reduce().expect("reduce returns");

    assert_eq!(
        harness.reports(),
        [ReduceReport {
            file: "src/b.ts".to_owned(),
            line: 4,
            column: 2,
            message: Some("unused export".to_owned()),
        }]
    );
}

#[test]
fn the_message_is_optional() {
    // `the_message_is_optional`. The engine falls back to the rule card's message, which is why
    // `None` has to survive as `None` rather than becoming an empty string.
    let mut harness = Harness::new(&["report", "src/b.ts", "1", "1"], Vec::new());
    harness.reduce().expect("reduce returns");

    assert_eq!(
        harness.reports(),
        [ReduceReport {
            file: "src/b.ts".to_owned(),
            line: 1,
            column: 1,
            message: None,
        }]
    );
}

#[test]
fn reporting_without_a_position_fails_the_call() {
    // `reporting_without_a_position_is_rejected`, and the decision Step 2 of this task had to
    // make: `wit/world.wit` declares `reduce-location`'s `line` and `column` as `option<u32>`,
    // so "did not say" is representable and something has to be decided about it.
    //
    // It is refused, matching QuickJS exactly. A cross-file violation with no site is
    // unactionable; the only stand-in available downstream is 1:1, which points a reader at an
    // unrelated line and cannot be told apart from a rule that meant 1:1. `report` declares no
    // error case, so failing the call is the refusal the boundary has — and under QuickJS the
    // equivalent throw becomes `RunError::Rule` and ends the run the same way.
    //
    // Each half independently: a report carrying a line and no column is as unactionable as one
    // carrying neither, and the world makes them two options rather than one optional pair.
    for probe in ["report-no-line", "report-no-column", "report-no-position"] {
        let mut harness = Harness::new(&[probe, "src/b.ts"], Vec::new());
        let error = harness
            .reduce()
            .expect_err("a report naming no site has no position to record");

        // `root_cause`, not the rendered error: wasmtime prefixes a host function's error with a
        // backtrace whose top frame is spelled `...[method]reduce-context.report`, so an
        // assertion on `format!("{error:?}")` passes whatever the host actually said.
        let cause = error.root_cause().to_string();
        assert!(
            cause.contains("`reduce-context.report`"),
            "the host names the method that could not be answered ({probe}): {cause}"
        );
        assert!(
            cause.contains("no line or column"),
            "and what was missing ({probe}): {cause}"
        );
        assert!(
            cause.contains("src/b.ts"),
            "and the file the rule did name, which is how the bad call is found ({probe}): \
             {cause}"
        );

        assert!(
            harness.reports().is_empty(),
            "a refused report is not recorded at a position the host invented ({probe})"
        );
    }
}

#[test]
fn taking_the_reports_empties_the_context() {
    // For the reason `take_facts` empties on the per-file side: a context read twice must not
    // report a violation twice.
    let mut harness = Harness::new(&["report", "src/b.ts", "1", "1", "once"], Vec::new());
    harness.reduce().expect("reduce returns");

    assert_eq!(harness.reports().len(), 1);
    assert!(harness.reports().is_empty(), "the second take sees nothing");
}

#[test]
fn a_forged_reduce_handle_traps_before_the_host_is_reached() {
    // The load-bearing layer of the phase split, and the fixture `wit/world.wit` says is owed:
    // the world's residual-weakness note was measured on a two-method prototype rather than on
    // this world, and this is the same forgery against the real thing.
    //
    // The guest names `reduce-context` — which it can, since both contexts live in one imported
    // interface — and forges a handle with the bindings' own `unsafe fn from_handle(0)`. Zero is
    // the handle it holds for its own phase, which is the most plausible forgery there is. The
    // component instance keeps a table per resource type and its `reduce-context` table is empty
    // for the length of a `check`, so the call dies in the runtime.
    let mut harness = Harness::new(&["files"], Vec::new());
    let error = harness
        .check("forge-reduce")
        .expect_err("a forged handle does not resolve");

    let cause = error.root_cause().to_string();
    assert!(
        cause.contains("unknown handle index"),
        "the runtime refused the handle, rather than the host refusing the call: {cause}"
    );

    // The host is never reached, and this is what says so rather than the message above: a
    // `report` that had landed would be sitting in the cross-file context.
    assert!(
        harness.reports().is_empty(),
        "the cross-file host recorded nothing, because it was never called"
    );

    // And the guest did reach the forgery, so the trap is not something that happened earlier.
    // Without this the test would pass for a fixture that failed to instantiate at all.
    assert_eq!(
        harness.check_messages(),
        ["forging a reduce-context handle"],
        "the announcement before the forgery, and not the claim of success after it"
    );
}

#[test]
fn a_forged_check_handle_traps_before_the_host_is_reached() {
    // The mirror image, and the other half of the split. A reduce phase that could emit facts
    // could feed itself, and there is no second pass for the result to reach.
    let mut harness = Harness::new(&["forge-check"], Vec::new());
    let error = harness
        .reduce()
        .expect_err("a forged handle does not resolve");

    let cause = error.root_cause().to_string();
    assert!(
        cause.contains("unknown handle index"),
        "the runtime refused the handle, rather than the host refusing the call: {cause}"
    );

    assert!(
        harness.check_facts().is_empty(),
        "the per-file host recorded no fact, because it was never called"
    );
    assert_eq!(
        harness.messages(),
        ["forging a check-context handle"],
        "the announcement before the forgery, and not the claim of success after it"
    );
}

#[test]
fn the_artifact_reaches_both_phases_through_their_own_resources() {
    // The boundary spelling, read off the built component rather than off this crate's bindings.
    // A component's embedded WIT is a *subset* of the world — it lists only what the guest calls
    // — so what this asserts is which resource each method the fixture reached belongs to.
    //
    // It is also the residual weakness read off an artifact: this guest's import list names
    // methods on *both* resources, which is exactly the "a per-file rule can name
    // `reduce-context`" the world states plainly. Naming is what it can do; the test above is
    // what happens when it tries to use one.
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, REDUCE).expect("the fixture is a valid component");
    let ty = component.component_type();

    let (_, types) = ty
        .imports(&engine)
        .find(|(name, _)| *name == "lanekeep:host/types@0.1.0")
        .expect("the world's one instance import is there");
    let ComponentItem::ComponentInstance(instance) = types.ty else {
        unreachable!("the world's import is an instance")
    };

    let mut methods: Vec<&str> = instance
        .exports(&engine)
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("[method]"))
        .collect();
    methods.sort_unstable();

    assert_eq!(
        methods,
        vec![
            "[method]check-context.emit-fact",
            "[method]check-context.report",
            "[method]check-context.root",
            "[method]reduce-context.facts",
            "[method]reduce-context.files",
            "[method]reduce-context.report",
        ],
        "exactly the six the fixture calls, three on each resource"
    );
    assert!(
        !methods.iter().any(|name| name.contains("query-subtree")),
        "a method the fixture never calls is absent from the artifact entirely"
    );
}
