//! A Go rule's map iteration order does not depend on how much work preceded it.
//!
//! This is the one property `go-rules/lanekeep`'s `Handlers` wrapper exists for, and the only
//! one in this repository that cannot be established by reading code. TinyGo randomizes map
//! iteration from `fastrand()`, a xorshift32 over **a single global that advances**
//! (`src/runtime/hashmap.go` lines 77, 294 and 402-403). On `-target=wasm-unknown` that global is
//! seeded to a constant — `hardwareRand` returns `(0, false)` — so there is no entropy in the
//! build at all, and the whole hazard is that a fixed seed is not a fixed *answer*: order depends
//! on the position in the cycle, and the position is wherever the previous call left it.
//!
//! `crates/lanekeep-wasm/wit/world.wit` fixes the instance lifetime at **one per (worker,
//! component)**, shared across every rule that component hosts and every file that worker
//! handles. So the draw count standing in front of any given `check` is a rayon work-stealing
//! artifact, and a rule that iterates a map produces output that varies between runs over
//! identical input with every cache-key input the same. That is what
//! `docs/architecture.md`'s determinism invariant forbids, and sorting violations by
//! `(ruleId, file, line, column)` does not rescue it: a rule choosing *which* node to report by
//! map order reports a different violation, not the same one in a different place.
//!
//! `lanekeep.Handlers` answers it by pinning both generators to their initial position at the top
//! of every host-called path. Nothing about a rule's source shows whether that happened, so the
//! evidence here is behavioral: drive one instance repeatedly and watch the order hold still.
//!
//! # All four host-called paths, not only `check`
//!
//! `Handlers` resets on `metadata`, `configure`, `check` and `reduce` alike, and the argument for
//! covering the two that are not passes is written out in `go-rules/lanekeep/handlers.go`: a
//! `configure` that decodes options into a `map[string]any` and ranges it is the ordinary way to
//! write one.
//!
//! This file probed `check` and nothing else for a while, against a fixture that declared its
//! metadata from constants, no `configure` and no `reduce` — so deleting the reset from any of
//! the other three was caught by nothing at all, while the comment in `handlers_test.go` said a
//! wasm probe caught all four. Each of the four reports an order of its own now, through
//! whichever channel that export has, and there is a test per path:
//!
//! | path | what carries the observation | test |
//! | --- | --- | --- |
//! | `check` | the violation message | [`a_go_rules_map_order_does_not_depend_on_the_work_that_preceded_it`] |
//! | `configure` | stored, and carried out by the next `check` | [`a_late_configure_sees_what_an_early_one_saw`] |
//! | `metadata` | the rule card's message | [`metadata_reports_one_map_order_however_much_ran_before_it`] |
//! | `reduce` | the cross-file violation message | [`reduce_reports_one_map_order_however_much_ran_before_it`] |
//!
//! `configure` is the one that needs more than a repeat call, because it runs once per (rule,
//! instance) on the way to that rule's first use. On a component hosting one rule it is therefore
//! always the first thing called on a fresh instance, where the generator is at its initial
//! position whether or not anything reset it. The fixture hosts two rules for that reason: rule
//! 1's `configure` can then be reached after rule 0 has worked the shared instance, which is
//! exactly the arrangement `crates/lanekeep-wasm/src/runtime.rs` produces under a real run.
//!
//! # Why the sensitivity test is not optional
//!
//! [`a_go_rules_map_order_does_not_depend_on_the_work_that_preceded_it`] is the property.
//! [`the_order_this_fixture_reports_moves_with_the_generator_position`] is what stops it passing
//! vacuously — and this test is very easy to write so that it passes vacuously. Whatever the
//! fixture reports has to be an observable that the generator's position can actually move: a
//! fixture that walked a slice, or sorted before reporting, or built a map too small for its
//! rotations to be distinct, would report the same thing on every call whether or not the reset
//! ran, and the first test would be green against a `ResetRand` that did nothing at all.
//!
//! So the fixture builds and walks its map three times per call and reports all three orders, and
//! the second test asserts they *disagree*. Those three walks stand at three different positions
//! in one call's own cycle, so their disagreeing is exactly the statement "this observable moves
//! with the generator position" — which is the premise every assertion here needs in order to
//! mean anything. It is stated once, of `check`'s three passes, and the other three paths report
//! the same observable through the same helper.
//!
//! # Measured rather than argued
//!
//! On TinyGo 0.41.1 against the fixture this recipe builds, with `go-rules/lanekeep`'s
//! `handlers.go` mutated one method at a time — `ResetRand()` deleted from exactly one of the
//! four, everything else left alone. Intact, every path on every call reports
//! `dfhacegb|acegbdfh|acegbdfh`, which is the order a freshly instantiated guest walks that map
//! in. Each mutant is caught by exactly one test:
//!
//! | reset deleted from | what moves | which test fails |
//! | --- | --- | --- |
//! | `Metadata` | the first three of the six calls: `bdfhaceg\|acegbdfh\|acegbdfh`, `acegbdfh\|gbdfhace\|cegbdfha`, `hacegbdf\|gbdfhace\|egbdfhac` | `metadata_reports_one_map_order_…` |
//! | `Configure` | rule 0's `dfhacegb\|acegbdfh\|acegbdfh` against rule 1's `bdfhaceg\|acegbdfh\|acegbdfh` | `a_late_configure_sees_what_an_early_one_saw` |
//! | `Check` | six calls give six messages, from `bdfhaceg\|acegbdfh\|acegbdfh` through `bdfhaceg\|dfhacegb\|acegbdfh` | `a_go_rules_map_order_does_not_depend_…` |
//! | `Reduce` | the first three of six, the same three the `Metadata` row lists | `reduce_reports_one_map_order_…` |
//!
//! **The two identical middle columns are the mechanism showing through**, not a copy-paste: a
//! neutered `metadata` and a neutered `reduce` both walk three maps per call and both start from
//! wherever the last resetting call left off, so they read the same three positions of one cycle.
//!
//! And the vacuous case was built too, because a guard nobody has watched fire is a guard nobody
//! has checked. With a reset neutered *and* `visit` walking the key slice rather than the map —
//! an observable that cannot move — the property tests go **green** over a broken SDK, and
//! [`the_order_this_fixture_reports_moves_with_the_generator_position`] is the only thing that
//! objects: `abcdefgh|abcdefgh|abcdefgh`.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]` modules
// and nothing else, so the helpers below — which are neither — need the grant restating. Only
// `expect_used` is listed because only it fires: nothing here panics directly, and an unfulfilled
// `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types::MatchEntry;
use lanekeep_wasm::host::{CheckContext, ReduceContext};
use lanekeep_wasm::{Resource, RuleSlot, WasmRuntime};

mod common;

/// The fixture, built by `just go-rules` from `go-rules/fixtures/maporder/`.
///
/// Named without the extension because that is what [`common::runtime_for`] takes; the artifact
/// is `tests/fixtures/go-maporder.wasm`, and it is recorded in `tests/go-component-digests.txt`
/// rather than in `tests/fixture-digests.txt` because `just wasm-fixtures` cannot rebuild it.
const FIXTURE: &str = "go-maporder";

/// How many times one instance is called.
///
/// Six rather than two. The claim is that the order does not depend on *how much* ran before, so
/// a single repeat would only rule out the first draw's worth of drift — and the fixture spends
/// nine draws per call, which is enough for two consecutive positions to coincide by luck but
/// not six.
const CALLS: usize = 6;

/// What the fixture puts between one pass's visit order and the next.
///
/// A `char` rather than a `&str` so it can be handed straight to `split`, and stated here rather
/// than inline because `go-rules/fixtures/maporder/main.go`'s `separator` is the other half of
/// the pair.
const SEPARATOR: char = '|';

/// What a `check` message puts between the whole of `configure`'s observation and the whole of
/// its own. The fixture's `section`.
const SECTION: char = '#';

/// How many visit orders one observation reports. `passes` in the fixture.
const PASSES: usize = 3;

/// One instance, called [`CALLS`] times, reports one order every time.
///
/// **One instance is the whole point, and it is counted rather than assumed.**
/// [`WasmRuntime::instantiations`] is the runtime's own evidence — a store builds at most one
/// instance per component and reuses it for every call — so asserting on it is what makes "after
/// N prior invocations *on that same instance*" a fact about this test rather than a claim in its
/// name. A harness that rebuilt the instance per call would reset the generator by accident and
/// pass against a `ResetRand` that did nothing.
///
/// The real `WasmRuntime` rather than `tests/world_shape.rs`'s stub host, for the same reason:
/// `check` there is the production dispatch, and instance reuse is a property of that path rather
/// than of the harness. The context is pushed once and lent to every call, which is also what
/// `lanekeep-engine` does — one context per file, lent to every rule checking it.
#[test]
fn a_go_rules_map_order_does_not_depend_on_the_work_that_preceded_it() {
    let (mut runtime, slot) = common::runtime_for(FIXTURE);
    let context = check_context(&mut runtime);

    let orders: Vec<String> = (0..CALLS)
        .map(|_| order(&mut runtime, slot, &context))
        .collect();

    assert_eq!(
        runtime.instantiations(),
        1,
        "every call has to land on one instance, or the generator is reset by accident and this \
         test asserts nothing"
    );

    let distinct: BTreeSet<&String> = orders.iter().collect();
    assert_eq!(
        distinct.len(),
        1,
        "one instance, {CALLS} calls, identical input: a Go rule's map order must be a function \
         of the call's own inputs and not of how many draws preceded it. Reported {orders:#?}"
    );
}

/// The order this fixture reports moves with the generator's position — so the test above is not
/// vacuous.
///
/// The fixture builds and walks the same map [`PASSES`] times in one `check` and reports every
/// order. Those walks stand at successive positions in the cycle, because building a map draws
/// once for its hash seed and walking one draws twice more for the iterator's start bucket and
/// start index. If they agreed, this fixture's observable would be independent of the position,
/// the reset would have nothing to do, and
/// [`a_go_rules_map_order_does_not_depend_on_the_work_that_preceded_it`] would be green over a
/// `ResetRand` that did nothing at all.
///
/// **Not all three distinct — at least two**, which is the strongest claim that is about the
/// mechanism rather than about one build. Three positions out of a 2^32-long cycle may land two
/// walks on the same permutation of eight keys without anything being wrong; three walks that all
/// agree is the failure this is looking for.
#[test]
fn the_order_this_fixture_reports_moves_with_the_generator_position() {
    let (mut runtime, slot) = common::runtime_for(FIXTURE);
    let context = check_context(&mut runtime);

    let reported = order(&mut runtime, slot, &context);
    // The half before the section marker is `configure`'s observation, which stands at a
    // different point in the cycle from any of this call's own passes and so says nothing about
    // whether *these* three move together.
    let (_, own) = reported
        .split_once(SECTION)
        .expect("the message leads with what `configure` saw");
    let passes: Vec<&str> = own.split(SEPARATOR).collect();
    assert_eq!(
        passes.len(),
        PASSES,
        "the fixture reports one visit order per pass, separated by `{SEPARATOR}`: {reported}"
    );

    let distinct: BTreeSet<&&str> = passes.iter().collect();
    assert!(
        distinct.len() > 1,
        "three walks of the same map at three positions in the generator's cycle came out \
         identical, so this fixture cannot tell a working reset from a missing one and the test \
         beside it is asserting nothing: {reported}"
    );

    // And the orders are orders: a pass that came back empty, or short, would also be "identical
    // to its neighbors" for a reason that has nothing to do with the generator.
    for pass in &passes {
        let bytes: BTreeSet<u8> = pass.bytes().collect();
        assert_eq!(
            bytes.len(),
            pass.len(),
            "a visit order names each key once: {pass}"
        );
        assert!(
            pass.len() > 1,
            "a one-key order cannot be permuted, so it could not move whatever the generator did: \
             {pass}"
        );
    }
}

/// A rule configured *late*, on an instance another rule has already worked, sees what a rule
/// configured on a fresh one sees.
///
/// This is the reset in `Handlers.Configure`, and nothing else here reaches it. `configure` runs
/// once per (rule, instance) on the way to that rule's first use, so with one rule it is always
/// the first call on a fresh instance and its observation is the initial position whether or not
/// anything reset it — a single-rule probe is green over a `Configure` with no reset at all.
///
/// The fixture hosts two rules, both the same rule, so the sequence below can put one
/// `configure` where the hazard actually puts it:
///
/// 1. reach rule 0, which configures it on a fresh instance and reports what its `configure` saw;
/// 2. run [`CALLS`] more checks on rule 0, every one of them advancing the shared generator;
/// 3. reach rule 1 for the first time, which is where *its* `configure` runs — on that same
///    instance, after all of the above.
///
/// The two observations have to agree. Under a real run the order of those steps is rayon's, so
/// disagreement is output that depends on the schedule with every cache-key input identical.
///
/// The module documentation's table has what each side reports with `ResetRand()` deleted from
/// `Handlers.Configure` alone, which is the mutation this test exists to fail on.
#[test]
fn a_late_configure_sees_what_an_early_one_saw() {
    let (mut harness, slots) = common::runtime_for_all(FIXTURE);
    let (early_rule, late_rule) = (slots[0], slots[1]);

    let early = configure_order(&mut harness, early_rule);
    for _ in 0..CALLS {
        let _ = configure_order(&mut harness, early_rule);
    }
    let late = configure_order(&mut harness, late_rule);

    assert_eq!(
        harness
            .runtime()
            .expect("the runtime is built")
            .instantiations(),
        1,
        "both rules have to share one instance, or the late `configure` runs on a fresh generator \
         and this test asserts nothing"
    );
    assert_eq!(
        early, late,
        "a rule configured after {CALLS} checks on the shared instance saw a different map order \
         than one configured on a fresh one"
    );
}

/// `metadata` reports one map order however much ran before it.
///
/// The rule card's message is the observation — the only string on that export a host reads back
/// verbatim. `metadata` is also the export with no error channel, so a wrong answer here is not
/// a failure: it is a rule describing itself differently on two workers.
///
/// **The calls are consecutive, and putting a `check` between them is what makes this test
/// assert nothing.** All four paths reset and then spend the same nine draws, so *every* call
/// that resets leaves the generator at the same place — which means an observation taken after
/// one is at that same place whether or not the export under test reset anything itself. The
/// first version of this test read `metadata`, ran [`CALLS`] checks, and read `metadata` again,
/// and it was green against a `Handlers.Metadata` with the reset deleted: both reads stood
/// behind a `check`'s reset and agreed for that reason. Back-to-back calls are the only
/// arrangement where a missing reset has anywhere to show, since the second then starts where
/// the first stopped.
///
/// The run after [`CALLS`] checks is kept as well, because it is the position a real run puts
/// this call at — it just cannot be the only one.
#[test]
fn metadata_reports_one_map_order_however_much_ran_before_it() {
    let (mut runtime, slot) = common::runtime_for(FIXTURE);
    let context = check_context(&mut runtime);

    let mut observed: Vec<String> = (0..CALLS)
        .map(|_| declared_order(&mut runtime, slot))
        .collect();
    for _ in 0..CALLS {
        order(&mut runtime, slot, &context);
    }
    observed.push(declared_order(&mut runtime, slot));

    assert_eq!(
        runtime.instantiations(),
        1,
        "every call has to land on one instance, or the generator is reset by accident"
    );
    let distinct: BTreeSet<&String> = observed.iter().collect();
    assert_eq!(
        distinct.len(),
        1,
        "a rule described itself differently depending on how many draws preceded the question. \
         Reported {observed:#?}"
    );
}

/// `reduce` reports one map order however much ran before it.
///
/// The cross-file pass runs on the instance that has just checked every file that worker was
/// handed, which is the largest amount of prior work any of the four paths meets. It is also the
/// pass whose output is most obviously order-sensitive: a rule reducing over facts it grouped in
/// a map reports a different violation, not the same one somewhere else.
///
/// Back-to-back calls, for the reason
/// [`metadata_reports_one_map_order_however_much_ran_before_it`] sets out: an observation taken
/// behind another resetting call is at that call's position rather than at its own, so a probe
/// with a `check` between its two reads is green over a `Handlers.Reduce` that resets nothing.
#[test]
fn reduce_reports_one_map_order_however_much_ran_before_it() {
    let (mut runtime, slot) = common::runtime_for(FIXTURE);
    let context = check_context(&mut runtime);
    let across = reduce_context(&mut runtime);

    let mut observed: Vec<String> = (0..CALLS)
        .map(|_| reduced_order(&mut runtime, slot, &across))
        .collect();
    for _ in 0..CALLS {
        order(&mut runtime, slot, &context);
    }
    observed.push(reduced_order(&mut runtime, slot, &across));

    assert_eq!(
        runtime.instantiations(),
        1,
        "every call has to land on one instance, or the generator is reset by accident"
    );
    let distinct: BTreeSet<&String> = observed.iter().collect();
    assert_eq!(
        distinct.len(),
        1,
        "a cross-file pass reported a different map order depending on how many draws preceded \
         it. Reported {observed:#?}"
    );
}

/// Run one rule's `check` and take the field of its message that `configure` filled in.
///
/// Reaching the rule is what configures it — `WasmRuntime::rule` is the only place `configure`
/// happens — so the first call for a given slot is both the configuration and the read of it.
fn configure_order(harness: &mut common::Ruleset, slot: RuleSlot) -> String {
    let runtime = harness.runtime().expect("the runtime is built");
    let context = check_context(runtime);
    let reported = order(runtime, slot, &context);
    let (configured, _) = reported
        .split_once(SECTION)
        .expect("the message leads with what `configure` saw");
    configured.to_owned()
}

/// Ask one rule what it is, and take the order its `metadata` visited a map in.
fn declared_order(runtime: &mut WasmRuntime, slot: RuleSlot) -> String {
    runtime
        .metadata(slot)
        .expect("the fixture describes itself without trapping")
        .card
        .message
}

/// Run one rule's cross-file pass and take the order it reported.
fn reduced_order(
    runtime: &mut WasmRuntime,
    slot: RuleSlot,
    context: &Resource<ReduceContext>,
) -> String {
    runtime
        .reduce(slot, context)
        .expect("the fixture's reduce returns without trapping");

    let mut reports = runtime
        .host_mut()
        .reduce_context_mut(context)
        .expect("the context outlives the call that borrowed it")
        .take_reports();
    assert_eq!(reports.len(), 1, "one report per call: {reports:?}");
    reports
        .pop()
        .and_then(|report| report.message)
        .expect("the fixture reports the visit order as the message")
}

/// A cross-file context over nothing, pushed once and lent to every call.
///
/// No files and no facts: the fixture reads neither, and a list it does not look at would be one
/// more thing for a reader to wonder about.
fn reduce_context(runtime: &mut WasmRuntime) -> Resource<ReduceContext> {
    runtime
        .host_mut()
        .push_reduce_context(ReduceContext::new(Vec::new(), Vec::new()))
        .expect("the resource table accepts a context")
}

/// Run the fixture once and take the message it reported.
///
/// One report per call, so a run that produced anything else is a fixture that stopped doing what
/// this file assumes rather than a property that failed — hence the count assertion rather than a
/// `first()`.
fn order(runtime: &mut WasmRuntime, slot: RuleSlot, context: &Resource<CheckContext>) -> String {
    // The fixture reads no capture — it reports at the root, whose handle is zero — so this is
    // the shape of a match rather than a match this rule branches on.
    let captures = vec![MatchEntry {
        name: "file".to_owned(),
        node: NodeArena::ROOT,
    }];
    runtime
        .check(slot, context, &captures)
        .expect("the fixture's check returns without trapping");

    let mut reports = runtime
        .host_mut()
        .check_context_mut(context)
        .expect("the context outlives the call that borrowed it")
        .take_reports();
    assert_eq!(reports.len(), 1, "one report per call: {reports:?}");
    reports
        .pop()
        .and_then(|report| report.message)
        .expect("the fixture reports the visit order as the message")
}

/// A context over one trivial file, pushed once and lent to every call.
///
/// TypeScript, because nothing here reads the tree and this is the parse every other fixture
/// harness in this crate uses. The fixture declares `typescript` for the same reason.
fn check_context(runtime: &mut WasmRuntime) -> Resource<CheckContext> {
    let source = "const x = 1;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the grammar loads");
    let tree = parser.parse(source, None).expect("it parses");

    runtime
        .host_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            "src/a.ts",
            Arc::new(TypeScript),
        ))
        .expect("the resource table accepts a context")
}
