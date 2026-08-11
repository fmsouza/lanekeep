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
//! # Why there are two tests and why the second one is not optional
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
//! with the generator position" — which is the premise the first test's assertion needs in order
//! to mean anything.
//!
//! Measured rather than argued, on TinyGo 0.41.1 against the fixture this commit builds. With
//! `ResetRand` intact, six calls on one instance report `dfhacegb|acegbdfh|acegbdfh` every time.
//! With its two assignments replaced by reads of the same globals, the same six calls report six
//! different messages:
//!
//! ```text
//! dfhacegb|acegbdfh|acegbdfh    bdfhaceg|acegbdfh|acegbdfh    acegbdfh|gbdfhace|cegbdfha
//! hacegbdf|gbdfhace|egbdfhac    egbdfhac|bdfhaceg|bdfhaceg    acegbdfh|dfhacegb|acegbdfh
//! ```
//!
//! The first of the six is the one the working build reports, which is the reset doing exactly
//! what it claims: pinning each call to the position a freshly instantiated guest starts from.
//!
//! And the vacuous case was built too, because a guard nobody has watched fire is a guard nobody
//! has checked. With the reset still neutered *and* `visit` walking the key slice rather than the
//! map — an observable that cannot move — the property test above goes **green** over a broken
//! SDK, and this file's second test is the only thing that objects:
//! `abcdefgh|abcdefgh|abcdefgh`.

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
use lanekeep_wasm::host::CheckContext;
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

/// How many visit orders one call reports. `passes` in the fixture.
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
    let passes: Vec<&str> = reported.split(SEPARATOR).collect();
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
