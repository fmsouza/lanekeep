//! Execution budgets.
//!
//! Turing-complete rules can fail to terminate. Three limits bound that, none of which can
//! be disabled: a per-invocation timeout, a global wall-clock budget for the whole run, and
//! a memory ceiling per runtime.
//!
//! Breaching any of them cancels the run — see `docs/architecture.md` §6.8 for why
//! continuing would be worse. Turning a breach into an error is each engine's own concern;
//! `lanekeep-js`'s `SandboxError` is one such type.
//!
//! # Why this lives in `lanekeep-core` rather than in one engine
//!
//! There is one global run budget, not one per engine: `docs/architecture.md`'s resource-limits
//! invariant is that breaching it cancels the *run*, and a run can call into more than one
//! engine (`lanekeep-js`'s QuickJS sandbox today, `lanekeep-wasm`'s component runtime once it
//! dispatches rules). [`RunClock`] is the shared origin that makes "the run" a single wall-clock
//! deadline rather than a per-engine one. Two independent clocks would each enforce their own
//! share of the budget correctly in isolation while the run as a whole overran both — a
//! quantitative failure, not a maintenance one, since it needs no drift to manifest: two honest
//! clocks that were never told about each other already sum past the one promise the run makes.
//! Defining `RunClock` once, here, is what keeps a second instance from being constructible at
//! all for a single run.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default budget for a single handler invocation.
pub const DEFAULT_RULE_TIMEOUT: Duration = Duration::from_secs(1);

/// Default wall-clock budget for an entire run.
pub const DEFAULT_GLOBAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Default memory ceiling per JavaScript runtime, which means per worker.
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Default wall-clock budget for host-side type-provider work across a whole run.
///
/// A minute rather than the fifteen seconds `DEFAULT_GLOBAL_TIMEOUT` gives guest execution,
/// because what this bounds is the user's own TypeScript program being built — a cost that
/// belongs to their project and scales with it, not with anything a rule did. That is also
/// why it is configurable where `COMPILE_BUDGET_PER_COMPONENT` is not: lanekeep's own
/// artifacts are lanekeep's problem, and a monorepo's `tsc` is not.
pub const DEFAULT_ANALYSIS_TIMEOUT: Duration = Duration::from_mins(1);

/// The three budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Budget for one handler invocation — a single `check` or `reduce` call.
    ///
    /// This is the limit that fires fast and names the culprit: which rule, which file,
    /// which phase. Keeping it well under the global budget means the diagnostic usually
    /// comes from the level that can identify the cause.
    pub rule_timeout: Duration,

    /// Wall-clock budget for the whole run.
    ///
    /// The backstop for when no single invocation is pathological but the aggregate is —
    /// a thousand rules each taking twenty milliseconds.
    pub global_timeout: Duration,

    /// Budget for host-side type-provider work across the whole run.
    ///
    /// **Analysis time, not elapsed time**: the sum of what the provider spends building
    /// programs and answering requests, as [`AnalysisBudget`] accumulates it, and not the
    /// wall clock since the run started. A run's own reading, parsing, matching and rule
    /// execution are `global_timeout`'s business and are charged nowhere here.
    ///
    /// Separate from `global_timeout` because it bounds host work rather than guest
    /// execution, and the two must not subsidize each other: a program build charged to the
    /// run budget would make a cold `tsc` run and a warm one take different exits over
    /// identical input, which is exactly the reasoning architecture §6.8 gives for taking
    /// component compilation off the run clock.
    pub analysis_timeout: Duration,

    /// Memory ceiling per runtime.
    pub memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rule_timeout: DEFAULT_RULE_TIMEOUT,
            global_timeout: DEFAULT_GLOBAL_TIMEOUT,
            analysis_timeout: DEFAULT_ANALYSIS_TIMEOUT,
            memory_bytes: DEFAULT_MEMORY_BYTES,
        }
    }
}

impl Limits {
    /// Raise the per-invocation budget, for a rule that legitimately does heavy work.
    ///
    /// Cannot raise the global budget: a single rule must not be able to extend the run's
    /// total. That is the whole point of having two levels rather than one.
    #[must_use]
    pub const fn with_rule_timeout(mut self, timeout: Duration) -> Self {
        self.rule_timeout = timeout;
        self
    }

    /// Set the global wall-clock budget.
    #[must_use]
    pub const fn with_global_timeout(mut self, timeout: Duration) -> Self {
        self.global_timeout = timeout;
        self
    }

    /// Set the per-runtime memory ceiling.
    #[must_use]
    pub const fn with_memory_bytes(mut self, bytes: usize) -> Self {
        self.memory_bytes = bytes;
        self
    }

    /// Set the analysis budget.
    #[must_use]
    pub const fn with_analysis_timeout(mut self, timeout: Duration) -> Self {
        self.analysis_timeout = timeout;
        self
    }
}

/// When the run started, shared by every worker.
///
/// The global budget has to be measured from one origin across all workers, or each would
/// enforce its own fifteen seconds and the run's total would scale with the worker count.
#[derive(Debug)]
pub struct RunClock {
    start: Instant,
    global_timeout: Duration,
}

impl RunClock {
    /// Start the clock now.
    #[must_use]
    pub fn start(global_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            global_timeout,
        })
    }

    /// How long the run has been going.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// The configured global budget.
    #[must_use]
    pub const fn global_timeout(&self) -> Duration {
        self.global_timeout
    }

    /// Whether the global budget is spent.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.elapsed() >= self.global_timeout
    }

    fn elapsed_nanos(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// An invocation whose clock is stopped, resumed when this drops.
///
/// A guard rather than a matched pair of calls, because the call it wraps can return early on
/// an error: a `disarm` whose `arm` sits after a `?` is a rule that runs unbounded from then
/// on, and nothing about the code would look wrong.
#[derive(Debug)]
pub struct Paused<'a> {
    budget: &'a Budget,
    remaining: u64,
    was_armed: bool,
}

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        if !self.was_armed {
            return;
        }
        let now = self.budget.clock.elapsed_nanos();
        // `.max(1)` for the reason `arm` uses it: zero means disarmed, so a resumed deadline
        // must never land on it.
        self.budget
            .invocation_deadline_nanos
            .store(now.saturating_add(self.remaining).max(1), Ordering::Relaxed);
    }
}

/// The run's budget for host-side type-provider work.
///
/// # Why this is a second clock rather than a share of [`RunClock`]
///
/// [`RunClock`] bounds guest execution and is polled from inside a handler by both engines.
/// A `tsc` provider's cost is neither: it is the user's own TypeScript program being built,
/// in a process lanekeep spawned, while no rule is running. Charging it to the run budget
/// would make a cold provider run and a warm one take different exits over identical input,
/// which is the determinism argument architecture §6.8 already makes for taking component
/// compilation off the run clock.
///
/// # Why an accumulator rather than a wall clock
///
/// **This budget is analysis time, not elapsed time.** An `Instant` taken at prepare charges
/// discovery, hashing, parsing, matching and every rule that ran to a budget whose own
/// breach message says it is "the cost of building the project's own TypeScript program and
/// not of running any rule" — so the message would be a lie, and a large corpus whose program
/// build takes half a minute would be cancelled for spending the rest of the minute doing the
/// work the run exists to do. Only what [`AnalysisBudget::charge`] brackets is charged.
///
/// It is the mirror image of [`Budget::pause`] in intent: the rule clock stops where the
/// analysis clock runs, so no instant is charged to both.
///
/// The converse does not hold, and that is deliberate. What a provider charges is *service*
/// time — the window in which its sidecar is working on one request — so an instant a worker
/// spends queued behind another worker's request is charged to neither clock. Charging the
/// queue wait instead would make the accumulator grow with the number of rayon workers rather
/// than with the work: measured through the `tsc` provider, fourteen workers each waiting
/// about 200 ms charged 2.866 s against 205 ms of wall clock, so a 60 s budget bounded roughly
/// 60/P seconds of real analysis and the breach message quoted a duration nobody could
/// observe. One sidecar serves the run, so the sum of its service times is the wall time it
/// was busy — which is exactly what "type analysis took …" claims to name.
///
/// `Clone` over a shared accumulator rather than `Copy` over an `Instant`: the engine holds
/// one and the provider holds another, and a charge on either has to be visible to the check
/// the other makes. Two clones are the same budget, not two budgets.
#[derive(Debug, Clone)]
pub struct AnalysisBudget {
    budget: Duration,
    /// Nanoseconds of analysis time charged so far, shared by every clone.
    spent: Arc<AtomicU64>,
}

impl AnalysisBudget {
    /// A budget with nothing spent yet.
    #[must_use]
    pub fn start(budget: Duration) -> Self {
        Self {
            budget,
            spent: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The configured budget, for a diagnostic.
    #[must_use]
    pub const fn budget(&self) -> Duration {
        self.budget
    }

    /// How much analysis time has been charged.
    #[must_use]
    pub fn spent(&self) -> Duration {
        Duration::from_nanos(self.spent.load(Ordering::Relaxed))
    }

    /// Charge everything until the returned guard drops to this budget.
    ///
    /// A guard rather than a matched pair for [`Paused`]'s reason: the call it brackets can
    /// return early on `?`, and a stop whose start sits after a `?` charges nothing for the
    /// one request that actually ran long.
    ///
    /// Nesting composes by over-charging rather than by being forbidden — an inner charge's
    /// time lands in the accumulator twice — so the caller brackets the outermost call it
    /// owns and nothing inside it.
    ///
    /// Bracket the *service*, never the wait for it. A provider whose sidecar serves one
    /// request at a time must take this guard after it holds whatever serializes access, or
    /// every worker queued behind the one being served charges the queue and the accumulator
    /// counts the same instants once per waiting thread. See the type's own documentation.
    #[must_use]
    pub fn charge(&self) -> Charge<'_> {
        Charge {
            budget: self,
            started: Instant::now(),
        }
    }

    /// How much of it is left, or `None` once it is breached.
    ///
    /// `Some(Duration::ZERO)` is a real answer and is not the same as `None`: a request handed
    /// a zero I/O timeout fails immediately and reports as a timeout, where `None` means the
    /// run is already over and nothing further should be attempted. The boundary is the same
    /// one [`analysis_overrun`] uses, so the two can never disagree about a single instant.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        remaining_after(self.spent(), self.budget)
    }

    /// The diagnostic, if the budget is spent.
    #[must_use]
    pub fn overrun(&self) -> Option<String> {
        analysis_overrun(self.spent(), self.budget)
    }
}

/// Analysis work whose time is being charged, added to the budget when this drops.
///
/// See [`AnalysisBudget::charge`].
#[derive(Debug)]
pub struct Charge<'a> {
    budget: &'a AnalysisBudget,
    started: Instant,
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        let nanos = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        // Saturating: a budget that has somehow accumulated most of a u64 of nanoseconds is
        // already over by nearly six centuries, and wrapping it back to nothing would be the
        // one arithmetic outcome that reads as a fresh budget.
        self.budget
            .spent
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |spent| {
                Some(spent.saturating_add(nanos))
            })
            .ok();
    }
}

/// The fallback detail for an analysis overrun that has no measurement to quote.
///
/// One function rather than a string written at each of the three sites that need it — the
/// provider's `begin_run` and `failure`, and the engine's `provider_for` — because
/// the engine's `RunError` carries a rendered message and nothing else, so three
/// spellings of one refusal would be three different pieces of advice for one fault.
///
/// Reached only when a request reported a timeout while the accumulator still reads short of
/// the budget: a request handed the last microsecond of it can outlive that microsecond
/// without the sum passing the total.
#[must_use]
pub fn analysis_overrun_fallback(budget: Duration) -> String {
    format!(
        "type analysis did not answer within the {budget:.1?} allowed\n  \
         this is the cost of building the project's own TypeScript program and not of running \
         any rule, so narrowing what is checked will not help much\n  \
         raise it with `timeouts.analysis`, or set `types.provider` to `builtin`"
    )
}

/// The arithmetic behind [`AnalysisBudget::remaining`], separated from the accumulator.
///
/// `budget.checked_sub(spent)` rather than `if spent > budget { None } else { Some(budget -
/// spent) }`: the two are equivalent everywhere except the exact point `spent == budget`,
/// where subtraction gives `Some(Duration::ZERO)` — spending precisely the budget still leaves a
/// (zero) answer, and only stepping past it turns the answer into `None`. Synthetic durations
/// drive that boundary directly; through [`AnalysisBudget`] only the `(0, 0)` corner is
/// reachable in practice, since a real [`Charge`] never measures exactly the budget.
pub(crate) fn remaining_after(spent: Duration, budget: Duration) -> Option<Duration> {
    budget.checked_sub(spent)
}

/// The arithmetic and the wording, separated from the accumulator.
///
/// A pure function for the reason `compile_overrun` is one: a test drives the comparison and
/// the message with synthetic microsecond `Duration`s, where an end-to-end test would have to
/// spend the whole budget to reach the branch. What no test asserts is that sixty seconds is
/// the right number of seconds — that is a judgment, and it is the user's to change, which is
/// exactly why this budget is configurable and `COMPILE_BUDGET_PER_COMPONENT` is not.
///
/// `spent` is analysis time — the sum of what [`AnalysisBudget::charge`] bracketed — and not
/// the time since the run started, which is what makes the message's second line true.
///
/// The message names `timeouts.analysis` and not `--timeout`. `--timeout` moves the *run*
/// budget, and printing advice that cannot work is the original `--timeout` bug in a new
/// phase — `AGENTS.md` records both instances.
#[must_use]
pub fn analysis_overrun(spent: Duration, budget: Duration) -> Option<String> {
    if spent <= budget {
        return None;
    }

    // The overrun printed beside the two figures, because `{:.1?}` rounds and two roundings
    // that land on the same tenth read as "took 3.0s, past the 3.0s allowed" — a message that
    // says a budget was breached and shows two identical numbers. The difference is computed
    // from the unrounded durations, so it is never zero here.
    let over = spent.saturating_sub(budget);
    Some(format!(
        "type analysis took {spent:.1?}, past the {budget:.1?} allowed (by {over:.3?})\n  \
         this is the cost of building the project's own TypeScript program and not of running \
         any rule, so narrowing what is checked will not help much\n  \
         raise it with `timeouts.analysis`, or set `types.provider` to `builtin`"
    ))
}

/// Which budget was breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trip {
    /// A single invocation ran too long.
    Rule,
    /// The run as a whole ran too long.
    Run,
}

const TRIP_NONE: u64 = 0;
const TRIP_RULE: u64 = 1;
const TRIP_RUN: u64 = 2;

/// Shared between an engine's runtime and its interrupt handler.
///
/// Records *why* execution was interrupted rather than leaving it to be inferred from the
/// engine's own exception or trap text. QuickJS, for instance, reports an interrupt as an
/// ordinary `Error` whose message happens to be "interrupted"; keying behavior off that
/// string would make the difference between "your rule looped forever" and "your rule
/// threw" depend on wording this project does not control. A different engine's own
/// interrupted-execution signal would be exactly as unreliable to string-match, for the
/// same reason.
#[derive(Debug)]
pub struct Budget {
    clock: Arc<RunClock>,
    global_nanos: u64,
    /// Deadline for the current invocation, in nanoseconds since the run started.
    /// Zero means no invocation is in flight.
    invocation_deadline_nanos: AtomicU64,
    tripped: AtomicU64,
}

impl Budget {
    /// Build a budget enforcer sharing the run's clock.
    pub fn new(clock: Arc<RunClock>) -> Arc<Self> {
        let global_nanos = u64::try_from(clock.global_timeout.as_nanos()).unwrap_or(u64::MAX);
        Arc::new(Self {
            clock,
            global_nanos,
            invocation_deadline_nanos: AtomicU64::new(0),
            tripped: AtomicU64::new(TRIP_NONE),
        })
    }

    /// Start the clock on one invocation.
    pub fn arm(&self, rule_timeout: Duration) {
        let now = self.clock.elapsed_nanos();
        let budget = u64::try_from(rule_timeout.as_nanos()).unwrap_or(u64::MAX);
        // Saturating: a deadline of zero means disarmed, so an overflowing budget must not
        // wrap around into it and silently switch the limit off.
        self.invocation_deadline_nanos
            .store(now.saturating_add(budget).max(1), Ordering::Relaxed);
        self.tripped.store(TRIP_NONE, Ordering::Relaxed);
    }

    /// Stop enforcing an invocation budget.
    pub fn disarm(&self) {
        self.invocation_deadline_nanos.store(0, Ordering::Relaxed);
    }

    /// Stop charging the current invocation while the host does work on its behalf.
    ///
    /// Returns a guard; the invocation resumes with exactly the time it had when the guard
    /// was taken, measured from wherever the run clock is when the guard drops.
    ///
    /// # Why not `disarm` followed by `arm`
    ///
    /// [`Budget::arm`] takes a fresh `rule_timeout` and also clears [`Budget::take_trip`]'s
    /// record. Re-arming after a provider call would therefore hand the rule a whole new
    /// allowance on every question it asks — a rule asking a hundred type questions would be
    /// bounded by nothing — and would erase a global-budget trip that had already been
    /// recorded, turning a run timeout into silence. This carries the remainder instead, so a
    /// rule's own budget still bounds a rule's own code and nothing else.
    ///
    /// The global check at [`Budget::should_interrupt`] is untouched: a paused invocation is
    /// still inside a run, and a run that has overrun must still stop.
    ///
    /// Nesting composes rather than being forbidden, which costs no runtime check: an inner
    /// pause reads an already stopped clock as disarmed and so restores nothing on drop,
    /// leaving the outermost guard — the only one holding a real remainder — to resume.
    #[must_use]
    pub fn pause(&self) -> Paused<'_> {
        let deadline = self.invocation_deadline_nanos.swap(0, Ordering::Relaxed);
        let now = self.clock.elapsed_nanos();
        Paused {
            budget: self,
            // Saturating, so a deadline already in the past resumes with nothing left rather
            // than wrapping into a very large allowance.
            remaining: deadline.saturating_sub(now),
            was_armed: deadline != 0,
        }
    }

    /// Whether execution should stop now, recording why. Called by the engine's interrupt
    /// handler, so it runs often and must stay cheap.
    pub fn should_interrupt(&self) -> bool {
        let elapsed = self.clock.elapsed_nanos();

        if elapsed >= self.global_nanos {
            self.tripped.store(TRIP_RUN, Ordering::Relaxed);
            return true;
        }

        let deadline = self.invocation_deadline_nanos.load(Ordering::Relaxed);
        if deadline != 0 && elapsed >= deadline {
            self.tripped.store(TRIP_RULE, Ordering::Relaxed);
            return true;
        }

        false
    }

    /// Which budget was breached, if any. Clears the record.
    pub fn take_trip(&self) -> Option<Trip> {
        match self.tripped.swap(TRIP_NONE, Ordering::Relaxed) {
            TRIP_RULE => Some(Trip::Rule),
            TRIP_RUN => Some(Trip::Run),
            _ => None,
        }
    }

    /// The run clock this budget was built from.
    pub fn clock(&self) -> &RunClock {
        &self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_budgets() {
        let limits = Limits::default();
        assert_eq!(limits.rule_timeout, Duration::from_secs(1));
        assert_eq!(limits.global_timeout, Duration::from_secs(15));
        assert_eq!(limits.memory_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn the_rule_budget_is_well_under_the_global_one() {
        // Not arithmetic for its own sake. If a single invocation could consume the whole
        // run, the global limit would be the one that fires, and its diagnostic cannot say
        // which rule or file was responsible.
        let limits = Limits::default();
        assert!(
            limits.rule_timeout * 5 < limits.global_timeout,
            "the per-invocation budget must leave room for the global limit to be a backstop"
        );
    }

    #[test]
    fn a_rule_cannot_raise_the_global_budget() {
        let limits = Limits::default().with_rule_timeout(Duration::from_mins(1));
        assert_eq!(limits.rule_timeout, Duration::from_mins(1));
        assert_eq!(
            limits.global_timeout, DEFAULT_GLOBAL_TIMEOUT,
            "raising a rule's own budget must not extend the run"
        );
    }

    #[test]
    fn an_unarmed_budget_never_interrupts() {
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        assert!(!budget.should_interrupt());
        assert_eq!(budget.take_trip(), None);
    }

    #[test]
    fn an_expired_invocation_budget_interrupts_and_records_why() {
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        budget.arm(Duration::ZERO);
        assert!(budget.should_interrupt());
        assert_eq!(budget.take_trip(), Some(Trip::Rule));
    }

    #[test]
    fn an_expired_run_budget_interrupts_and_records_why() {
        let budget = Budget::new(RunClock::start(Duration::ZERO));
        budget.arm(Duration::from_hours(1));
        assert!(budget.should_interrupt());
        assert_eq!(budget.take_trip(), Some(Trip::Run));
    }

    #[test]
    fn the_run_budget_wins_when_both_are_spent() {
        // The run being over is the more consequential fact: every subsequent invocation
        // will breach too, so reporting the rule budget would name an arbitrary victim.
        let budget = Budget::new(RunClock::start(Duration::ZERO));
        budget.arm(Duration::ZERO);
        assert!(budget.should_interrupt());
        assert_eq!(budget.take_trip(), Some(Trip::Run));
    }

    #[test]
    fn disarming_stops_invocation_enforcement() {
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        budget.arm(Duration::ZERO);
        budget.disarm();
        assert!(!budget.should_interrupt(), "no invocation is in flight");
    }

    #[test]
    fn taking_the_trip_clears_it() {
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        budget.arm(Duration::ZERO);
        assert!(budget.should_interrupt());
        assert_eq!(budget.take_trip(), Some(Trip::Rule));
        assert_eq!(
            budget.take_trip(),
            None,
            "a trip must not be reported twice"
        );
    }

    #[test]
    fn arming_clears_a_previous_trip() {
        // Otherwise the next invocation would inherit the last one's verdict and be
        // reported as timing out without ever running.
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        budget.arm(Duration::ZERO);
        assert!(budget.should_interrupt());

        budget.arm(Duration::from_hours(1));
        assert!(!budget.should_interrupt());
        assert_eq!(budget.take_trip(), None);
    }

    #[test]
    fn an_overflowing_budget_does_not_wrap_into_disarmed() {
        // A deadline of zero means "no invocation in flight". An enormous budget must
        // saturate rather than wrap around to zero and switch the limit off entirely.
        let budget = Budget::new(RunClock::start(Duration::from_hours(1)));
        budget.arm(Duration::MAX);
        assert_ne!(
            budget.invocation_deadline_nanos.load(Ordering::Relaxed),
            0,
            "an overflowing budget must not read as disarmed"
        );
    }

    #[test]
    fn the_clock_measures_from_one_origin() {
        let clock = RunClock::start(Duration::from_hours(1));
        let a = Arc::clone(&clock);
        let b = Arc::clone(&clock);
        assert!(!a.is_expired());
        assert!(!b.is_expired());
        assert_eq!(a.global_timeout(), Duration::from_hours(1));
    }

    #[test]
    fn a_zero_global_budget_is_immediately_expired() {
        assert!(RunClock::start(Duration::ZERO).is_expired());
    }

    #[test]
    fn the_analysis_budget_defaults_to_a_minute() {
        assert_eq!(Limits::default().analysis_timeout, Duration::from_mins(1));
        assert_eq!(DEFAULT_ANALYSIS_TIMEOUT, Duration::from_mins(1));
    }

    #[test]
    fn the_analysis_budget_is_settable_without_moving_the_others() {
        let limits = Limits::default().with_analysis_timeout(Duration::from_secs(5));
        assert_eq!(limits.analysis_timeout, Duration::from_secs(5));
        assert_eq!(limits.rule_timeout, DEFAULT_RULE_TIMEOUT);
        assert_eq!(limits.global_timeout, DEFAULT_GLOBAL_TIMEOUT);
    }

    const ANALYSIS: Duration = Duration::from_mins(1);

    #[test]
    fn analysis_within_budget_is_not_an_overrun() {
        assert_eq!(analysis_overrun(Duration::ZERO, ANALYSIS), None);
        // The boundary is inclusive, matching `compile_overrun`: spending exactly the budget
        // is spending the budget, not exceeding it.
        assert_eq!(analysis_overrun(ANALYSIS, ANALYSIS), None);
    }

    #[test]
    fn analysis_one_microsecond_past_the_budget_is_an_overrun() {
        assert!(analysis_overrun(ANALYSIS + Duration::from_micros(1), ANALYSIS).is_some());
    }

    #[test]
    fn the_analysis_overrun_names_analysis_and_the_setting_that_raises_it() {
        let detail =
            analysis_overrun(ANALYSIS * 3, ANALYSIS).expect("three times the budget is an overrun");
        assert!(detail.contains("type analysis"), "got: {detail}");
        assert!(detail.contains("timeouts.analysis"), "got: {detail}");
        // Not `--timeout`: that raises the *run* budget, and advice that cannot work is the
        // exact failure `AGENTS.md` records for the original `--timeout` bug.
        assert!(!detail.contains("--timeout"), "got: {detail}");
    }

    /// A breach whose two figures round to the same tenth still reads as a breach.
    ///
    /// `{:.1?}` rounds, so `3.04s` past `3.0s` printed "took 3.0s, past the 3.0s allowed" — a
    /// message asserting a budget was exceeded and showing two identical numbers, which reads
    /// as a lanekeep bug rather than as a slow project. The overrun is computed from the
    /// unrounded durations, so it is never zero when this branch is reached at all.
    #[test]
    fn an_overrun_too_small_for_the_rounding_is_still_printed() {
        let budget = Duration::from_secs(3);
        let detail = analysis_overrun(budget + Duration::from_millis(40), budget)
            .expect("forty milliseconds past the budget is an overrun");
        assert!(
            detail.contains("took 3.0s, past the 3.0s allowed"),
            "{detail}"
        );
        assert!(
            detail.contains("(by 40.000ms)"),
            "the two rounded figures are equal, so the difference has to be printed: {detail}"
        );
    }

    // `remaining_after` is `AnalysisBudget::remaining`'s arithmetic with the clock removed, so
    // the boundary can be driven with synthetic durations: two real `Instant::now()` reads are
    // never equal (see `remaining_after`'s doc comment), so no test built on `AnalysisBudget`
    // itself can land on `elapsed == budget` to exercise it.
    #[test]
    fn remaining_after_at_zero_elapsed_and_zero_budget_is_zero_not_none() {
        assert_eq!(
            remaining_after(Duration::ZERO, Duration::ZERO),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn remaining_after_one_nanosecond_past_a_zero_budget_is_none() {
        assert_eq!(
            remaining_after(Duration::from_nanos(1), Duration::ZERO),
            None
        );
    }

    #[test]
    fn remaining_after_short_of_the_budget_is_the_gap() {
        assert_eq!(
            remaining_after(Duration::from_secs(59), Duration::from_mins(1)),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn remaining_after_exactly_the_budget_is_zero_not_none() {
        assert_eq!(
            remaining_after(Duration::from_mins(1), Duration::from_mins(1)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn remaining_after_one_nanosecond_past_the_budget_is_none() {
        assert_eq!(
            remaining_after(
                Duration::from_mins(1) + Duration::from_nanos(1),
                Duration::from_mins(1)
            ),
            None
        );
    }

    // These drive `AnalysisBudget`'s accumulator. Wide margins throughout: `sleep` guarantees
    // a floor and nothing else, so every assertion is one-sided.
    #[test]
    fn a_generous_budget_has_remaining_time_no_greater_than_the_budget() {
        let generous = AnalysisBudget::start(Duration::from_hours(1));
        let remaining = generous.remaining().expect("an hour is not spent yet");
        assert!(remaining <= Duration::from_hours(1));
        assert!(generous.overrun().is_none());
    }

    #[test]
    fn a_charge_adds_the_time_it_measured() {
        let budget = AnalysisBudget::start(Duration::from_mins(1));
        assert_eq!(budget.spent(), Duration::ZERO, "nothing has been charged");
        {
            let _charge = budget.charge();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            budget.spent() >= Duration::from_millis(20),
            "a charge adds what it measured, got {:?}",
            budget.spent()
        );
    }

    #[test]
    fn two_charges_add() {
        let budget = AnalysisBudget::start(Duration::from_mins(1));
        for _ in 0..2 {
            let _charge = budget.charge();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            budget.spent() >= Duration::from_millis(40),
            "two charges accumulate rather than replacing one another, got {:?}",
            budget.spent()
        );
    }

    #[test]
    fn nothing_is_spent_while_no_charge_is_open() {
        // The whole ruling in one test: the budget is analysis time, so a run that spends
        // fifty milliseconds discovering, hashing, parsing and matching has spent none of it.
        let budget = AnalysisBudget::start(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(budget.spent(), Duration::ZERO);
        assert_eq!(budget.remaining(), Some(Duration::from_millis(1)));
        assert_eq!(budget.overrun(), None);
    }

    #[test]
    fn remaining_and_overrun_read_the_accumulator() {
        let budget = AnalysisBudget::start(Duration::from_millis(1));
        {
            let _charge = budget.charge();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(budget.remaining(), None, "a millisecond is long gone");
        let detail = budget.overrun().expect("the budget is spent");
        assert!(detail.contains("timeouts.analysis"), "got: {detail}");
    }

    #[test]
    fn a_clone_is_the_same_accumulator_rather_than_a_second_one() {
        // The engine holds one and the provider holds another; a charge on either has to be
        // visible to the check the other makes, or the engine would bound a budget nothing
        // spends.
        let budget = AnalysisBudget::start(Duration::from_mins(1));
        let copy = budget.clone();
        {
            let _charge = copy.charge();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(budget.spent() >= Duration::from_millis(20));
        assert_eq!(budget.spent(), copy.spent());
    }

    #[test]
    fn a_paused_invocation_is_not_charged_for_the_host_work_it_waited_on() {
        // A rule with a 30 ms budget asks a provider a question that takes 40 ms of host
        // time; that host time must not count against the rule's own budget.
        let clock = RunClock::start(Duration::from_mins(1));
        let budget = Budget::new(clock);
        budget.arm(Duration::from_millis(300));
        assert!(!budget.should_interrupt(), "nothing has run yet");

        {
            let _paused = budget.pause();
            std::thread::sleep(Duration::from_millis(400));
            assert!(
                !budget.should_interrupt(),
                "host work while paused is not the rule's"
            );
        }

        // Resumed with what it had, not with a fresh allowance.
        assert!(!budget.should_interrupt(), "the rule has its budget back");
        std::thread::sleep(Duration::from_millis(450));
        assert!(
            budget.should_interrupt(),
            "and it is still bounded - a second pause would not have refilled it"
        );
        assert_eq!(budget.take_trip(), Some(Trip::Rule));
    }

    #[test]
    fn a_pause_taken_late_resumes_with_what_was_left() {
        // Every other pause test arms and pauses back-to-back, so `remaining == deadline`
        // passes them too - the subtraction never actually has anything to subtract. Spend
        // part of the budget before pausing, so a `remaining: deadline` mutant (dropping the
        // `.saturating_sub(now)`) hands the rule back its whole original allowance instead of
        // what was left, and is caught here instead of shipping unnoticed.
        let clock = RunClock::start(Duration::from_mins(1));
        let budget = Budget::new(clock);
        // Seconds rather than tens of milliseconds: a hosted CI runner overshoots a sleep by
        // a hundred milliseconds or more under load, and the first assertion below holds only
        // while the overshoot stays under what was left when the pause began.
        budget.arm(Duration::from_secs(2));
        std::thread::sleep(Duration::from_secs(1));

        {
            let _paused = budget.pause();
            std::thread::sleep(Duration::from_millis(1500));
        }

        // ~1 s of the original 2 s was left when it paused; none of the 1.5 s of host work
        // while paused counts against it.
        assert!(
            !budget.should_interrupt(),
            "the remainder has not run out yet"
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            budget.should_interrupt(),
            "the remainder it resumed with is now spent"
        );
        assert_eq!(budget.take_trip(), Some(Trip::Rule));
    }

    #[test]
    fn pausing_a_disarmed_budget_leaves_it_disarmed() {
        // Config load and the reduce phase both run with nothing armed. A guard that re-armed
        // on drop regardless would invent a per-invocation deadline where there was none.
        let clock = RunClock::start(Duration::from_mins(1));
        let budget = Budget::new(clock);
        drop(budget.pause());
        std::thread::sleep(Duration::from_millis(5));
        assert!(!budget.should_interrupt());
    }

    #[test]
    fn a_pause_does_not_clear_a_recorded_trip() {
        // `arm` resets `tripped`; this must not, or the trip that decides whether a breach is
        // reported as a rule timeout or a run timeout would be erased by a provider call.
        let clock = RunClock::start(Duration::from_millis(1));
        let budget = Budget::new(clock);
        std::thread::sleep(Duration::from_millis(5));
        assert!(budget.should_interrupt());
        drop(budget.pause());
        assert_eq!(budget.take_trip(), Some(Trip::Run));
    }

    #[test]
    fn a_pause_does_not_suspend_the_run_clock() {
        // "Limits cancel the run; they never degrade it." A paused rule clock stops charging
        // the *rule*; the run as a whole is still running, and a run that overruns while the
        // host is building a program must still stop. Otherwise a provider call would be a
        // hole in the global budget large enough to drive a whole analysis through.
        let clock = RunClock::start(Duration::from_millis(5));
        let budget = Budget::new(clock);
        budget.arm(Duration::from_hours(1));
        let _paused = budget.pause();
        std::thread::sleep(Duration::from_millis(10));
        assert!(budget.should_interrupt(), "the run budget is spent");
        assert_eq!(budget.take_trip(), Some(Trip::Run));
    }

    #[test]
    fn nested_pauses_compose_and_the_outermost_one_decides() {
        // Nesting is not forbidden, because forbidding it would mean a runtime check on a
        // path that has none today. It composes instead: the inner pause reads an already
        // stopped clock as disarmed, so its drop restores nothing and the outer guard - the
        // one holding the real remainder - is what puts the deadline back.
        let clock = RunClock::start(Duration::from_mins(1));
        let budget = Budget::new(clock);
        budget.arm(Duration::from_millis(30));

        let outer = budget.pause();
        {
            let _inner = budget.pause();
            std::thread::sleep(Duration::from_millis(40));
        }
        assert!(
            !budget.should_interrupt(),
            "the inner guard must not resume the clock"
        );
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !budget.should_interrupt(),
            "and it must not have re-armed a stale deadline either"
        );
        drop(outer);

        assert!(!budget.should_interrupt(), "resumed with its 30 ms");
        std::thread::sleep(Duration::from_millis(45));
        assert!(budget.should_interrupt(), "and still bounded by them");
        assert_eq!(budget.take_trip(), Some(Trip::Rule));
    }
}
