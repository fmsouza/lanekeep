//! The three limits, enforced against a real component.
//!
//! One case per case in `crates/lanekeep-js/src/sandbox.rs`'s limit tests, named the same,
//! because the limits are the same: `lanekeep_core::limits` is shared, not copied, and a
//! reader comparing the two engines should be comparing behavior rather than vocabulary. The
//! guest is `tests/fixtures/limits/`, which offers a rule that never terminates, one that
//! never stops allocating, one that returns immediately, and one that does a host-chosen
//! amount of real work.
//!
//! # What each assertion has to survive
//!
//! **A timing test is the easiest kind to write so that it cannot fail.** A budget test passes
//! trivially if the work finished before the clock mattered, and it looks identical to one
//! that passed because enforcement worked. Three things guard against that here.
//!
//! Every "must be stopped" case uses a probe that cannot finish on its own, or one whose
//! finishing is itself asserted against — so a limit that did nothing produces a hang or a
//! recorded report, never a quiet pass. Every classification is asserted by variant rather
//! than by message, since the recorded `Trip` is what distinguishes the three and the trap
//! text is wasmtime's to change. And [`the_budget_can_be_raised_as_well_as_lowered`] exists
//! because `AGENTS.md` records a flag that was validated and then dropped: a test that only
//! *lowers* a limit passes against that bug, since a run completes either way.
//!
//! # The two cases at the end are not correctness tests
//!
//! [`the_run_budget_is_not_checked_between_invocations`] records a **known gap**, closed by the
//! plan's Task 17 and not by this module. Its assertion is expected to change when that lands.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` is listed because only it fires: nothing here panics
// directly, and an unfulfilled `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;
use std::time::Duration;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::host::{CheckContext, ReduceContext, ReduceReport, Report};
use lanekeep_wasm::runtime::WasmEngine;
use lanekeep_wasm::{WasmError, WasmRuntime};
use wasmtime::component::Resource;

/// The component under test, as built by `just wasm-fixtures`.
const LIMITS: &[u8] = include_bytes!("fixtures/limits.wasm");

/// The path the context reports as the file under check. Nothing here reads it.
const FILE: &str = "src/example.ts";

/// A runtime with one instance of the limits fixture in it, and one context to lend.
///
/// The context outlives each call, on the terms `tests/reads.rs`'s harness sets out: what the
/// engine will eventually build per file and what it builds per rule is still open, and a
/// harness that rebuilt one per call could not observe anything that accumulates.
struct Run {
    runtime: WasmRuntime,
    rule: Rule,
    check: Resource<CheckContext>,
}

impl Run {
    /// A run under the given limits, with its own engine, ticker and clock.
    fn new(limits: Limits) -> Self {
        let (run, _) = Self::build(limits, None, EPOCH_DEFAULT);
        run
    }

    /// A run whose clock starts with `budget` left, returned so a case can wait it out.
    ///
    /// **The clock starts after the component is compiled, and that ordering is the point.**
    /// Compiling this fixture takes a couple of seconds in a test build, which is longer than
    /// any budget a case can afford to wait out — so a clock started before it is spent before
    /// the harness finishes assembling itself, and every case fails during setup rather than
    /// where it means to. That is not a lanekeep bug: compilation genuinely counts against the
    /// run in a real run too. It is a harness that has to put the expensive step outside the
    /// window it is measuring.
    fn expiring_in(limits: Limits, budget: Duration) -> (Self, Arc<RunClock>) {
        Self::build(limits, Some(budget), EPOCH_DEFAULT)
    }

    /// As [`Run::expiring_in`], on an engine whose epoch advances at a chosen interval.
    fn expiring_in_with_tick(
        limits: Limits,
        budget: Duration,
        interval: Duration,
    ) -> (Self, Arc<RunClock>) {
        Self::build(limits, Some(budget), interval)
    }

    fn build(
        limits: Limits,
        budget: Option<Duration>,
        interval: Duration,
    ) -> (Self, Arc<RunClock>) {
        let engine =
            WasmEngine::with_tick_interval(interval).expect("the shipped configuration builds");
        let component = engine
            .compile(LIMITS)
            .expect("the fixture is a valid component");

        // Everything above is outside the budget on purpose; everything below is inside it.
        let clock = RunClock::start(budget.unwrap_or(limits.global_timeout));
        let mut runtime =
            WasmRuntime::new(engine, limits, Arc::clone(&clock)).expect("the world links");
        let rule = runtime.instantiate(&component).expect("instantiates");

        let source = "const x = 1;\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let context = CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            FILE,
            Arc::new(TypeScript),
        );
        let check = runtime
            .host_mut()
            .push_check_context(context)
            .expect("the resource table accepts a context");

        (
            Self {
                runtime,
                rule,
                check,
            },
            clock,
        )
    }

    /// Run the per-file pass for a probe, under the runtime's default budget.
    fn check(&mut self, probe: &[&str]) -> Result<(), WasmError> {
        let captures = entries(probe);
        self.runtime.call_check(&self.rule, &self.check, &captures)
    }

    /// Run the per-file pass for a probe under an explicit budget.
    fn check_within(&mut self, probe: &[&str], timeout: Duration) -> Result<(), WasmError> {
        let captures = entries(probe);
        self.runtime
            .call_check_with_timeout(&self.rule, &self.check, &captures, timeout)
    }

    /// Run the cross-file pass, with the probe name as the first file.
    fn reduce(&mut self, probe: &str) -> Result<(), WasmError> {
        let context = ReduceContext::new(vec![probe.to_owned()], Vec::new());
        let handle = self
            .runtime
            .host_mut()
            .push_reduce_context(context)
            .expect("the resource table accepts a context");
        self.runtime.call_reduce(&self.rule, &handle)
    }

    /// Everything the per-file pass has reported since this was last called.
    fn reports(&mut self) -> Vec<Report> {
        let handle = Resource::new_own(self.check.rep());
        self.runtime
            .host_mut()
            .check_context_mut(&handle)
            .expect("the context is still live")
            .take_reports()
    }

    /// The messages of everything reported since this was last called.
    fn messages(&mut self) -> Vec<String> {
        self.reports()
            .into_iter()
            .map(|report| report.message.unwrap_or_default())
            .collect()
    }
}

/// How much of the run's budget a case that means to exhaust it starts with.
///
/// `RunClock::start(Duration::ZERO)` is the obvious spelling of "the run is already over", and
/// it is the wrong one here: instantiating a component is itself a budgeted guest call, so a
/// clock that is spent at construction fails the harness rather than the case. The run expires
/// the honest way instead — a small budget, waited out — which also makes these cases stronger
/// than a born-expired clock would, since what they exercise is a run that ran out partway
/// through.
const NEARLY_SPENT: Duration = Duration::from_millis(25);

/// The tick interval `Run::new` uses, which is the shipped one.
const EPOCH_DEFAULT: Duration = lanekeep_wasm::runtime::EPOCH_TICK_INTERVAL;

/// Wait until the run budget really is spent, rather than assuming it.
fn wait_out(clock: &RunClock) {
    while !clock.is_expired() {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A probe name and its arguments, as the `match` a `check` call receives.
///
/// The node is zero throughout — the root, and the one handle every context has. Nothing in
/// this fixture navigates, so what crosses is the list of names.
fn entries(probe: &[&str]) -> Vec<types::MatchEntry> {
    probe
        .iter()
        .map(|name| types::MatchEntry {
            name: (*name).to_owned(),
            node: 0,
        })
        .collect()
}

// --- the six cases `lanekeep-js` names -------------------------------------------------

#[test]
fn sandbox_a_rule_that_never_terminates_is_stopped() {
    let mut run = Run::new(Limits::default().with_rule_timeout(Duration::from_millis(120)));

    let err = run.check(&["spin"]).expect_err("must be stopped");
    assert!(
        matches!(err, WasmError::RuleTimeout { .. }),
        "expected a rule timeout, got {err:?}"
    );
    assert!(err.is_limit_breach());
    assert!(
        run.runtime.epoch_fired(),
        "the epoch deadline callback is the mechanism; if it never ran, something else \
         stopped the guest and this test is not measuring what it claims"
    );
}

#[test]
fn sandbox_a_tight_allocation_loop_is_stopped() {
    // Four mebibytes, against a fixture whose linear memory starts at 1,114,112 bytes: enough
    // that instantiation fits comfortably and a rule collecting 64 KiB blocks does not.
    let mut run = Run::new(
        Limits::default()
            .with_memory_bytes(4 * 1024 * 1024)
            .with_rule_timeout(Duration::from_secs(10)),
    );

    let err = run.check(&["grow"]).expect_err("must be stopped");
    assert!(
        matches!(
            err,
            WasmError::MemoryExceeded {
                limit_bytes: 4_194_304
            }
        ),
        "expected a memory breach naming the ceiling, got {err:?}"
    );
    assert!(err.is_limit_breach());
    // That it is `MemoryExceeded` at all is the proof that it took the limiter's path rather
    // than the epoch one: `classify` only ever produces this variant from the limiter's own
    // breach record, and a growth refusal records no `Trip`. Nothing is asserted about the
    // epoch callback here — it fires every millisecond regardless of whether it interrupts,
    // so its having run says nothing either way.
}

#[test]
fn sandbox_the_run_budget_stops_execution_even_with_a_generous_rule_budget() {
    // The backstop case: nothing about this invocation is pathological relative to its own
    // budget, but the run is already over.
    let (mut run, clock) = Run::expiring_in(
        Limits::default().with_rule_timeout(Duration::from_hours(1)),
        NEARLY_SPENT,
    );
    wait_out(&clock);

    let err = run.check(&["spin"]).expect_err("must be stopped");
    assert!(
        matches!(err, WasmError::RunTimeout { .. }),
        "expected a run timeout, got {err:?}"
    );
}

/// **The one case where the two engines genuinely differ, and the difference is wasmtime's.**
///
/// `lanekeep-js`'s test of this name evaluates `1 + 1` after a breach and expects `2`: a
/// QuickJS context is reusable once an interrupt has unwound it. A wasmtime `Store` is not.
/// Any trap out of wasm sets a **store-wide** flag — `store.0.set_trapped()` in
/// `wasmtime-47.0.3/src/runtime/func.rs:1478`, on every error path out of
/// `invoke_wasm_and_catch_traps` — and every component call is gated on `!trapped()`, in
/// `src/runtime/component/concurrent_disabled.rs:181`. There is no public way to clear it.
///
/// Note the scope, because the message misstates it: the trap reads `cannot enter component
/// instance` and the flag is not per instance. A second, untouched rule instantiated into the
/// same store is equally dead.
///
/// That is acceptable, and it is acceptable for the reason the invariant gives rather than by
/// luck: **a breach cancels the run, so there is no next call to make.** What still has to
/// hold is that the runtime is not left unable to *report* — this is not a poison that eats
/// the diagnostic. Both halves are asserted below, so the day wasmtime makes a trapped store
/// reusable, this fails and says which claim moved.
#[test]
fn sandbox_survives_a_breach_and_keeps_working() {
    let mut run = Run::new(Limits::default().with_rule_timeout(Duration::from_millis(80)));

    run.check(&["tick"]).expect("an ordinary call first");

    let err = run.check(&["spin"]).expect_err("must be stopped");
    assert!(matches!(err, WasmError::RuleTimeout { .. }), "{err:?}");

    // The half that survives: everything the guest reported before it was stopped is still
    // readable, and the host state was not taken down with the trap.
    assert_eq!(
        run.messages(),
        vec!["tick".to_owned()],
        "a breach must not cost the reports that preceded it"
    );

    // The half that does not, stated as an assertion rather than as a comment.
    let err = run
        .check(&["tick"])
        .expect_err("a trapped store cannot be re-entered");
    assert!(
        matches!(&err, WasmError::Guest { message } if message.contains("cannot enter")),
        "expected wasmtime's re-entry refusal, got {err:?}"
    );

    // And a fresh runtime is unaffected, so the poison is the store's and not the engine's.
    let mut next = Run::new(Limits::default());
    next.check(&["tick"]).expect("a new store is clean");
    assert_eq!(next.messages(), vec!["tick".to_owned()]);
}

#[test]
fn sandbox_a_breach_is_not_reported_twice() {
    // If the trip record survived, the next invocation would be reported as timing out
    // without having run at all. Here that is exactly what distinguishes the two: the call
    // after a breach fails for its own reason — wasmtime refuses to re-enter a trapped store,
    // per the case above — and it must be classified as that rather than as a second timeout.
    let mut run = Run::new(Limits::default().with_rule_timeout(Duration::from_millis(80)));

    assert!(matches!(
        run.check(&["spin"]),
        Err(WasmError::RuleTimeout { .. })
    ));

    let err = run.check(&["tick"]).expect_err("the store is trapped");
    assert!(
        matches!(err, WasmError::Guest { .. }),
        "the second failure has to be judged on its own; a leaked `Trip` would make it a \
         second `RuleTimeout` for a call that never ran: {err:?}"
    );
}

#[test]
fn sandbox_a_per_rule_budget_overrides_the_default() {
    let mut run = Run::new(Limits::default().with_rule_timeout(Duration::from_hours(1)));

    let err = run
        .check_within(&["spin"], Duration::from_millis(80))
        .expect_err("the explicit budget applies");
    assert!(matches!(err, WasmError::RuleTimeout { .. }), "{err:?}");
}

// --- the assertions a lowering-only test would have missed ------------------------------

#[test]
fn the_budget_can_be_raised_as_well_as_lowered() {
    // `AGENTS.md`: a `--timeout` that was validated and then never applied survived because
    // the failure has no symptom of its own — lowering a budget looks like it worked, since
    // the run completes either way. Only raising one distinguishes an applied budget from an
    // ignored one, so both halves are asserted here against the same guest.
    //
    // The raised call goes first and on its own runtime, because a trapped store cannot be
    // re-entered — see `sandbox_survives_a_breach_and_keeps_working`.
    let generous = Limits::default()
        .with_rule_timeout(Duration::from_millis(50))
        .with_global_timeout(Duration::from_mins(10));

    let mut raised = Run::new(generous);
    raised
        .check_within(&["work", "400"], Duration::from_mins(2))
        .expect("the same work fits in a budget raised to two minutes");
    assert_eq!(
        raised.messages().len(),
        1,
        "the raised budget has to have let it finish, not merely returned Ok"
    );

    let mut lowered = Run::new(generous);
    let err = lowered
        .check(&["work", "400"])
        .expect_err("400 million iterations must not fit in the default 50ms");
    assert!(matches!(err, WasmError::RuleTimeout { .. }), "{err:?}");
    assert!(
        lowered.messages().is_empty(),
        "a stopped call must not have reached its report"
    );
}

#[test]
fn the_cross_file_pass_is_bounded_by_the_same_budget() {
    // `reduce` runs once per rule rather than once per match, which makes it the invocation
    // most likely to be quietly left unbudgeted. It is the same `arm`/`disarm` pair.
    let mut run = Run::new(Limits::default().with_rule_timeout(Duration::from_millis(120)));

    let err = run.reduce("spin").expect_err("must be stopped");
    assert!(matches!(err, WasmError::RuleTimeout { .. }), "{err:?}");
}

#[test]
fn a_well_behaved_cross_file_pass_still_reports() {
    // The control for the case above: if `reduce` were failing for some reason other than the
    // budget, that test would pass and say nothing.
    let mut run = Run::new(Limits::default());

    let context = ReduceContext::new(vec!["quiet".to_owned()], Vec::new());
    let handle = run
        .runtime
        .host_mut()
        .push_reduce_context(context)
        .expect("the resource table accepts a context");
    run.runtime
        .call_reduce(&run.rule, &handle)
        .expect("an ordinary reduce runs");

    let owned = Resource::new_own(handle.rep());
    let reports: Vec<ReduceReport> = run
        .runtime
        .host_mut()
        .reduce_context_mut(&owned)
        .expect("the context is still live")
        .take_reports();
    assert_eq!(
        reports
            .iter()
            .map(|r| r.message.clone())
            .collect::<Vec<_>>(),
        vec![Some("reduced".to_owned())]
    );
}

#[test]
fn the_ceiling_applies_at_instantiation_before_a_rule_has_run_a_line() {
    // Each instance of this fixture claims 1,114,112 bytes — a 17-page minimum linear memory —
    // the moment it is instantiated, which makes instantiation the first thing that can breach
    // the ceiling. It has to be classified as a breach rather than as a broken artifact, or the
    // diagnostic sends the reader to the rule component instead of to the budget.
    const PER_INSTANCE: usize = 1_114_112;
    let ceiling = PER_INSTANCE - 65_536;

    let mut runtime =
        WasmRuntime::with_limits(Limits::default().with_memory_bytes(ceiling)).expect("builds");
    let component = runtime.engine().compile(LIMITS).expect("valid component");

    // `let Err(..) else` rather than `expect_err`, because the generated `Rule` has no
    // `Debug` — `bindgen!` writes none — and `expect_err` needs one for the success type.
    let Err(err) = runtime.instantiate(&component) else {
        panic!("one instance already crosses a ceiling smaller than its own minimum memory");
    };
    assert!(
        matches!(err, WasmError::MemoryExceeded { .. }),
        "expected a memory breach, got {err:?}"
    );
}

/// **The reason `MemoryCeiling` is hand-written instead of `StoreLimitsBuilder::memory_size`.**
///
/// wasmtime's helper compares each linear memory against the ceiling on its own and knows
/// nothing about the others in the store. A store holds one instance per rule, so under it a
/// worker running twenty rules could reach twenty times the budget it was given, silently.
///
/// Three instances of a 1,114,112-byte fixture against a ceiling of two of them plus a page:
/// the first two fit and the third does not. Under a per-memory ceiling all three would fit,
/// which is what makes this discriminating rather than decorative.
#[test]
fn a_second_instance_is_charged_against_the_same_ceiling_as_the_first() {
    const PER_INSTANCE: usize = 1_114_112;
    let mut runtime =
        WasmRuntime::with_limits(Limits::default().with_memory_bytes(PER_INSTANCE * 2 + 65_536))
            .expect("builds");
    let component = runtime.engine().compile(LIMITS).expect("valid component");

    runtime.instantiate(&component).expect("the first fits");
    runtime.instantiate(&component).expect("the second fits");
    let Err(err) = runtime.instantiate(&component) else {
        panic!("the third instance crosses the summed ceiling");
    };
    assert!(matches!(err, WasmError::MemoryExceeded { .. }), "{err:?}");
}

#[test]
fn a_trap_that_is_not_a_limit_breach_is_not_reported_as_one() {
    // The negative side of `classify`. Without it, any failure at all would look like a
    // breach and `is_limit_breach` would carry no information.
    let mut run = Run::new(Limits::default());
    run.check(&["nonsense"]).expect("an unknown probe reports");
    assert_eq!(
        run.messages(),
        vec!["unknown probe `nonsense`".to_owned()],
        "the guest ran to completion, so nothing here is a breach"
    );
}

// --- the recorded gap, and its control ---------------------------------------------------

/// **This records a known gap. It is not a correctness test, and its assertion is expected to
/// change.**
///
/// `AGENTS.md`: the global run budget is polled by QuickJS's interrupt handler, so it only
/// bounds a run while JavaScript is executing — four hundred files against a one-line rule ran
/// to completion under a one-millisecond budget. Switching engines does not touch that. The
/// epoch check Cranelift emits lives at function entries and loop back-edges *inside guest
/// code*, so wall-clock time passing between invocations is invisible to it for exactly the
/// same reason. The plan's Task 17 adds the outer clock check that closes it, in the engine
/// above both runtimes, because both need it.
///
/// # Why the tick interval is pinned rather than left at its default
///
/// At the shipped one-millisecond interval this demonstration would be a race rather than a
/// fact: a tick landing inside any one of the four hundred calls stops the sequence, and with
/// roughly two microseconds of guest time per call that happens with probability about
/// `400 × 2µs / 1ms`, which is most of the time. So the interval is set beyond the life of the
/// test, which makes the structural claim exactly: with no tick inside any call, the budget is
/// never consulted at all, and the run overruns without limit.
///
/// That pinning is also what makes the claim falsifiable rather than vacuous, because
/// [`the_run_budget_does_stop_a_guest_that_keeps_executing`] runs the same fixture and the same
/// expired clock against a ticker that does tick — so a green result here cannot be explained
/// by the limit being unwired.
#[test]
fn the_run_budget_is_not_checked_between_invocations() {
    let (mut run, clock) = Run::expiring_in_with_tick(
        Limits::default().with_rule_timeout(Duration::from_hours(1)),
        NEARLY_SPENT,
        Duration::from_hours(1),
    );
    wait_out(&clock);
    assert!(
        clock.is_expired(),
        "the run's budget is spent before a call is made"
    );

    for call in 0..400 {
        run.check(&["tick"]).unwrap_or_else(|e| {
            panic!("call {call} was stopped, which Task 17 has not landed yet: {e:?}")
        });
    }

    assert_eq!(
        run.messages().len(),
        400,
        "four hundred invocations ran to completion with the run budget already spent"
    );
    assert!(
        !run.runtime.epoch_fired(),
        "and the budget was never so much as consulted"
    );
}

/// The control for the case above: the limit is wired, and a guest that keeps executing is
/// stopped by it.
///
/// The same fixture and the same already-expired clock, differing only in that the guest does
/// not return promptly and the ticker does tick. `work` terminates on its own after two
/// billion iterations, deliberately — a probe that could only be stopped by the runtime would
/// turn "the limit did nothing" into a hang rather than into a failed assertion.
#[test]
fn the_run_budget_does_stop_a_guest_that_keeps_executing() {
    let (mut run, clock) = Run::expiring_in(
        Limits::default().with_rule_timeout(Duration::from_hours(1)),
        NEARLY_SPENT,
    );
    wait_out(&clock);

    let err = run
        .check(&["work", "2000"])
        .expect_err("a guest that keeps executing is stopped");
    assert!(
        matches!(err, WasmError::RunTimeout { .. }),
        "expected a run timeout, got {err:?}"
    );
    assert!(run.messages().is_empty(), "and it did not reach its report");
}
