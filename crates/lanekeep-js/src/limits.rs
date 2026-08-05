//! Execution budgets.
//!
//! Turing-complete rules can fail to terminate. Three limits bound that, none of which can
//! be disabled: a per-invocation timeout, a global wall-clock budget for the whole run, and
//! a memory ceiling per runtime.
//!
//! Breaching any of them cancels the run. See [`crate::SandboxError`] for why continuing
//! would be worse.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default budget for a single handler invocation.
pub const DEFAULT_RULE_TIMEOUT: Duration = Duration::from_secs(1);

/// Default wall-clock budget for an entire run.
pub const DEFAULT_GLOBAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Default memory ceiling per JavaScript runtime, which means per worker.
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

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

    /// Memory ceiling per runtime.
    pub memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rule_timeout: DEFAULT_RULE_TIMEOUT,
            global_timeout: DEFAULT_GLOBAL_TIMEOUT,
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

/// Which budget was breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trip {
    /// A single invocation ran too long.
    Rule,
    /// The run as a whole ran too long.
    Run,
}

const TRIP_NONE: u64 = 0;
const TRIP_RULE: u64 = 1;
const TRIP_RUN: u64 = 2;

/// Shared between the sandbox and its interrupt handler.
///
/// Records *why* execution was interrupted rather than leaving it to be inferred from the
/// engine's exception text. The engine reports an interrupt as an ordinary `Error` whose
/// message happens to be "interrupted"; keying behavior off that string would make the
/// difference between "your rule looped forever" and "your rule threw" depend on wording
/// this project does not control.
#[derive(Debug)]
pub(crate) struct Budget {
    clock: Arc<RunClock>,
    global_nanos: u64,
    /// Deadline for the current invocation, in nanoseconds since the run started.
    /// Zero means no invocation is in flight.
    invocation_deadline_nanos: AtomicU64,
    tripped: AtomicU64,
}

impl Budget {
    pub(crate) fn new(clock: Arc<RunClock>) -> Arc<Self> {
        let global_nanos = u64::try_from(clock.global_timeout.as_nanos()).unwrap_or(u64::MAX);
        Arc::new(Self {
            clock,
            global_nanos,
            invocation_deadline_nanos: AtomicU64::new(0),
            tripped: AtomicU64::new(TRIP_NONE),
        })
    }

    /// Start the clock on one invocation.
    pub(crate) fn arm(&self, rule_timeout: Duration) {
        let now = self.clock.elapsed_nanos();
        let budget = u64::try_from(rule_timeout.as_nanos()).unwrap_or(u64::MAX);
        // Saturating: a deadline of zero means disarmed, so an overflowing budget must not
        // wrap around into it and silently switch the limit off.
        self.invocation_deadline_nanos
            .store(now.saturating_add(budget).max(1), Ordering::Relaxed);
        self.tripped.store(TRIP_NONE, Ordering::Relaxed);
    }

    /// Stop enforcing an invocation budget.
    pub(crate) fn disarm(&self) {
        self.invocation_deadline_nanos.store(0, Ordering::Relaxed);
    }

    /// Whether execution should stop now, recording why. Called by the engine's interrupt
    /// handler, so it runs often and must stay cheap.
    pub(crate) fn should_interrupt(&self) -> bool {
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
    pub(crate) fn take_trip(&self) -> Option<Trip> {
        match self.tripped.swap(TRIP_NONE, Ordering::Relaxed) {
            TRIP_RULE => Some(Trip::Rule),
            TRIP_RUN => Some(Trip::Run),
            _ => None,
        }
    }

    pub(crate) fn clock(&self) -> &RunClock {
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
}
