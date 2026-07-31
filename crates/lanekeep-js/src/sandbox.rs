//! The sandbox: a JavaScript runtime with no ambient authority and enforced budgets.

use std::path::Path;
use std::sync::Arc;

use lanekeep_lang::Language;
use rquickjs::context::intrinsic;
use rquickjs::promise::PromiseState;
use rquickjs::{CatchResultExt, Context, Ctx, FromJs, Module, Runtime};

use crate::error::SandboxError;
use crate::host::HostContext;
use crate::limits::{Budget, Limits, RunClock, Trip};
use crate::loader::{LoadedModules, RuleLoader, RuleResolver, RuleRoot};

/// The intrinsics rule code gets.
///
/// This is an allowlist, and that is the point. Three of the engine's optional intrinsics
/// are sources of nondeterminism, and leaving them out means there is no original for a
/// rule to reach — nothing to patch, nothing to restore, no prototype chain leading back to
/// the real thing:
///
/// - `Date` — `Date.now()` and `new Date()` read the system clock.
/// - `Performance` — `performance.now()` is a clock by another name, and the one most
///   likely to be forgotten when reasoning about "we removed `Date`".
/// - `WeakRef` — `WeakRef` and `FinalizationRegistry` make garbage-collection timing
///   observable, which is nondeterminism that does not look like a clock at all.
///
/// `Eval` is included, and has to be: the engine's own script evaluation depends on it, so
/// omitting it makes the sandbox unable to run anything. It grants a rule no capability it
/// lacks — a rule is already arbitrary code — though it does mean rule review cannot rely
/// on reading the source alone.
type SandboxedIntrinsics = (
    intrinsic::Eval,
    intrinsic::RegExpCompiler,
    intrinsic::RegExp,
    intrinsic::Json,
    intrinsic::Proxy,
    intrinsic::MapSet,
    intrinsic::TypedArrays,
    intrinsic::Promise,
);

/// Removes what the intrinsic allowlist cannot.
///
/// `Math.random` lives in the base objects, which are not optional, so it has to be deleted
/// after the fact. `SharedArrayBuffer` and `Atomics` are useless without threads and are
/// removed to keep the surface small rather than because a concrete attack is known.
///
/// Deleting is enough: a rule may define its own `Math.random`, but whatever it writes is
/// its own code and therefore deterministic. What it cannot do is reach the engine's
/// entropy source, because the only reference to it is gone.
const BOOTSTRAP: &str = r"
    'use strict';
    delete Math.random;
    delete globalThis.SharedArrayBuffer;
    delete globalThis.Atomics;
";

/// A JavaScript runtime that rule code executes in.
///
/// Not `Sync`: the underlying engine runtime is single-threaded, so each rayon worker owns
/// one. They share a [`RunClock`] so the global budget is measured once for the run rather
/// than once per worker.
pub struct Sandbox {
    runtime: Runtime,
    context: Context,
    limits: Limits,
    budget: Arc<Budget>,
    loaded: Option<LoadedModules>,
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sandbox")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Sandbox {
    /// Build a sandbox sharing a run clock.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Engine`] if the runtime cannot be created or the bootstrap
    /// fails, both of which indicate a broken build rather than anything about a rule.
    pub fn new(limits: Limits, clock: Arc<RunClock>) -> Result<Self, SandboxError> {
        let runtime = Runtime::new().map_err(|e| SandboxError::Engine(e.to_string()))?;
        runtime.set_memory_limit(limits.memory_bytes);

        let context = Context::custom::<SandboxedIntrinsics>(&runtime)
            .map_err(|e| SandboxError::Engine(e.to_string()))?;

        // The bootstrap runs before the interrupt handler is installed, deliberately. It is
        // host code rather than rule code, and a sandbox has to be constructible even when
        // the run budget is already spent — otherwise an expired run reports "could not
        // build a sandbox" instead of the timeout it actually is, which sends the reader
        // looking for a broken installation.
        context.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP)
                .catch(&ctx)
                .map_err(|e| SandboxError::Engine(format!("bootstrap failed: {e}")))
        })?;

        let budget = Budget::new(clock);
        let handler_budget = Arc::clone(&budget);
        runtime.set_interrupt_handler(Some(Box::new(move || handler_budget.should_interrupt())));

        Ok(Self {
            runtime,
            context,
            limits,
            budget,
            loaded: None,
        })
    }

    /// Build a sandbox that can load rule modules from a rules root.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::new`].
    pub fn with_modules(
        limits: Limits,
        clock: Arc<RunClock>,
        root: RuleRoot,
        typescript: Arc<dyn Language>,
        javascript: Arc<dyn Language>,
    ) -> Result<Self, SandboxError> {
        let mut sandbox = Self::new(limits, clock)?;
        let loader = RuleLoader::new(root.clone(), typescript, javascript);
        sandbox.loaded = Some(loader.loaded());
        sandbox.runtime.set_loader(RuleResolver::new(root), loader);
        Ok(sandbox)
    }

    /// Every module loaded so far, when this sandbox was built with a module root.
    ///
    /// The hash of the rule graph is derived from this, so it has to reflect what was
    /// actually read rather than what the config named.
    #[must_use]
    pub fn loaded_modules(&self) -> Option<&LoadedModules> {
        self.loaded.as_ref()
    }

    /// Import a rule module and return its default export.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] on a breached budget, a module that fails to resolve or
    /// load, or a module that throws while evaluating.
    pub fn import_default<T>(&self, path: &Path) -> Result<T, SandboxError>
    where
        T: for<'js> FromJs<'js>,
    {
        self.budget.arm(self.limits.rule_timeout);
        let outcome = self.context.with(|ctx| {
            let promise = match Module::import(&ctx, path.display().to_string()) {
                Ok(promise) => promise,
                Err(err) => return Err(capture_failure(&ctx, &err)),
            };

            // The engine is synchronous and the loader does no I/O asynchronously, so an
            // import settles during this call. Draining the job queue is still required —
            // module evaluation is scheduled as a job rather than run inline.
            while promise.state() == PromiseState::Pending && ctx.execute_pending_job() {}

            match promise.finish::<rquickjs::Object<'_>>() {
                Ok(namespace) => namespace
                    .get::<_, T>("default")
                    .map_err(|err| capture_failure(&ctx, &err)),
                Err(err) => Err(capture_failure(&ctx, &err)),
            }
        });
        self.budget.disarm();

        outcome.map_err(|raw| self.classify(&raw, self.limits.rule_timeout))
    }

    /// Build a sandbox with its own run clock, starting now.
    ///
    /// For a single-threaded run or a test. A real run shares one clock across workers.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::new`].
    pub fn with_limits(limits: Limits) -> Result<Self, SandboxError> {
        let clock = RunClock::start(limits.global_timeout);
        Self::new(limits, clock)
    }

    /// The budgets in force.
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Evaluate source, enforcing the per-invocation budget.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] on a breached budget or a thrown value. Every variant
    /// cancels the run.
    pub fn eval<T>(&self, source: &str) -> Result<T, SandboxError>
    where
        T: for<'js> FromJs<'js>,
    {
        self.eval_with_timeout(source, self.limits.rule_timeout)
    }

    /// Evaluate source under an explicit per-invocation budget, for a rule that declared
    /// its own.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::eval`].
    pub fn eval_with_timeout<T>(
        &self,
        source: &str,
        timeout: std::time::Duration,
    ) -> Result<T, SandboxError>
    where
        T: for<'js> FromJs<'js>,
    {
        // Re-armed per invocation rather than left running. The engine polls the interrupt
        // handler every so many instructions, so a stale deadline would let a short handler
        // slip through while stopping a long one at an arbitrary point.
        self.budget.arm(timeout);
        let outcome = self.context.with(|ctx| match ctx.eval::<T, _>(source) {
            Ok(value) => Ok(value),
            Err(err) => Err(capture_failure(&ctx, &err)),
        });
        self.budget.disarm();

        // Classification happens outside `with`, and has to: reading the runtime's memory
        // usage while the context still holds its borrow panics on a double borrow. The
        // split exists for that reason rather than for tidiness.
        outcome.map_err(|raw| self.classify(&raw, timeout))
    }

    /// Evaluate a synthetic module under a chosen name.
    ///
    /// The name matters: the resolver treats it as the importing module's path, so a
    /// synthetic entry has to sit inside the rules root for relative specifiers in its
    /// source to resolve. Naming it outside would make every import look like an escape.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::eval`].
    pub fn eval_module(&self, name: &str, source: &str) -> Result<(), SandboxError> {
        self.budget.arm(self.limits.rule_timeout);
        let outcome = self.context.with(|ctx| {
            let promise = match Module::evaluate(ctx.clone(), name, source) {
                Ok(promise) => promise,
                Err(err) => return Err(capture_failure(&ctx, &err)),
            };
            while promise.state() == PromiseState::Pending && ctx.execute_pending_job() {}
            promise
                .finish::<()>()
                .map_err(|err| capture_failure(&ctx, &err))
        });
        self.budget.disarm();

        outcome.map_err(|raw| self.classify(&raw, self.limits.rule_timeout))
    }

    /// Evaluate source with a `ctx` object in scope, the way a rule handler runs.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::eval`].
    pub fn eval_with_host<T>(&self, host: &HostContext, source: &str) -> Result<T, SandboxError>
    where
        T: for<'js> FromJs<'js>,
    {
        self.eval_with_host_timeout(host, source, self.limits.rule_timeout)
    }

    /// Evaluate with `ctx` in scope under an explicit budget, for a rule that declared one.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::eval`].
    pub fn eval_with_host_timeout<T>(
        &self,
        host: &HostContext,
        source: &str,
        timeout: std::time::Duration,
    ) -> Result<T, SandboxError>
    where
        T: for<'js> FromJs<'js>,
    {
        self.budget.arm(timeout);
        let outcome = self.context.with(|ctx| {
            let object = match host.build(&ctx) {
                Ok(object) => object,
                Err(err) => return Err(capture_failure(&ctx, &err)),
            };
            if let Err(err) = ctx.globals().set("ctx", object) {
                return Err(capture_failure(&ctx, &err));
            }
            match ctx.eval::<T, _>(source) {
                Ok(value) => Ok(value),
                Err(err) => Err(capture_failure(&ctx, &err)),
            }
        });
        self.budget.disarm();

        outcome.map_err(|raw| self.classify(&raw, timeout))
    }

    /// Turn a raw failure into something that says what actually happened.
    fn classify(&self, raw: &RawFailure, timeout: std::time::Duration) -> SandboxError {
        // Our own record first. The engine reports an interrupt as an ordinary Error whose
        // message happens to read "interrupted", and keying off that string would make the
        // difference between "looped forever" and "threw" depend on wording this project
        // does not control.
        match self.budget.take_trip() {
            Some(Trip::Run) => {
                return SandboxError::RunTimeout {
                    budget: self.budget.clock().global_timeout(),
                    elapsed: self.budget.clock().elapsed(),
                };
            }
            Some(Trip::Rule) => return SandboxError::RuleTimeout { budget: timeout },
            None => {}
        }

        let (message, stack, was_error_object) = match raw {
            RawFailure::Engine(detail) => return SandboxError::Engine(detail.clone()),
            RawFailure::Exception {
                message,
                stack,
                was_error_object,
            } => (message, stack, *was_error_object),
        };

        // Memory exhaustion surfaces as an ordinary exception. The reliable tell is that
        // the runtime is sitting at its ceiling, since the allocation that failed is the
        // one that would have crossed it — so usage rests just below the line, not at it.
        let used = u64::try_from(self.runtime.memory_usage().malloc_size).unwrap_or(0);
        let ceiling = u64::try_from(self.limits.memory_bytes).unwrap_or(u64::MAX);
        let at_ceiling = ceiling > 0 && used.saturating_mul(10) >= ceiling.saturating_mul(9);

        if at_ceiling && (!was_error_object || message.contains("out of memory")) {
            return SandboxError::MemoryExceeded {
                limit_bytes: self.limits.memory_bytes,
            };
        }
        if !was_error_object {
            return SandboxError::NonErrorThrown;
        }

        SandboxError::Script {
            message: message.clone(),
            stack: stack.clone(),
        }
    }
}

/// What the engine reported, before deciding what it means.
///
/// Extracted inside the context borrow; interpreted outside it.
enum RawFailure {
    Engine(String),
    Exception {
        message: String,
        stack: Option<String>,
        was_error_object: bool,
    },
}

fn capture_failure(ctx: &Ctx<'_>, err: &rquickjs::Error) -> RawFailure {
    if !matches!(err, rquickjs::Error::Exception) {
        return RawFailure::Engine(err.to_string());
    }

    let caught = ctx.catch();
    caught.as_exception().map_or_else(
        || RawFailure::Exception {
            message: String::new(),
            stack: None,
            was_error_object: false,
        },
        |exception| RawFailure::Exception {
            message: exception.message().unwrap_or_default(),
            stack: exception.stack(),
            was_error_object: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn sandbox() -> Sandbox {
        Sandbox::with_limits(Limits::default()).expect("sandbox builds")
    }

    /// `typeof x` for a global, without tripping a `ReferenceError`.
    fn type_of(sandbox: &Sandbox, expression: &str) -> String {
        sandbox
            .eval::<String>(&format!("typeof ({expression})"))
            .unwrap_or_else(|e| {
                // A ReferenceError here also means "absent", which is what the caller asked.
                let _ = e;
                "undefined".to_owned()
            })
    }

    #[test]
    fn evaluates_ordinary_javascript() {
        let s = sandbox();
        assert_eq!(s.eval::<i32>("1 + 1").expect("evaluates"), 2);
        assert_eq!(
            s.eval::<String>("[3,1,2].sort().join('-')")
                .expect("evaluates"),
            "1-2-3"
        );
        assert_eq!(
            s.eval::<i32>("function add(a,b){return a+b}; add(20, 22)")
                .expect("evaluates"),
            42
        );
    }

    #[test]
    fn keeps_what_rules_actually_need() {
        let s = sandbox();
        for global in [
            "JSON", "RegExp", "Map", "Set", "Promise", "Proxy", "BigInt", "Math",
        ] {
            assert_ne!(
                type_of(&s, global),
                "undefined",
                "{global} should be available"
            );
        }
        assert_eq!(
            s.eval::<String>(r"JSON.stringify({a:1})").expect("json"),
            "{\"a\":1}"
        );
        assert!(s.eval::<bool>(r"/^ab+c$/.test('abbbc')").expect("regexp"));
    }

    // --- absence of ambient authority ------------------------------------------------

    #[test]
    fn there_is_no_filesystem_or_process_access() {
        let s = sandbox();
        for global in [
            "fs",
            "process",
            "require",
            "child_process",
            "module",
            "__dirname",
            "Deno",
            "Bun",
        ] {
            assert_eq!(type_of(&s, global), "undefined", "{global} must not exist");
        }
    }

    #[test]
    fn there_is_no_network_access() {
        let s = sandbox();
        for global in [
            "fetch",
            "XMLHttpRequest",
            "WebSocket",
            "navigator",
            "Request",
            "Response",
        ] {
            assert_eq!(type_of(&s, global), "undefined", "{global} must not exist");
        }
    }

    #[test]
    fn there_are_no_timers() {
        // Timers have no meaning in a synchronous single-pass engine, and `setTimeout`
        // would be a scheduling primitive a rule could use to observe wall-clock time.
        let s = sandbox();
        for global in [
            "setTimeout",
            "setInterval",
            "setImmediate",
            "requestAnimationFrame",
        ] {
            assert_eq!(type_of(&s, global), "undefined", "{global} must not exist");
        }
    }

    // --- absence of nondeterminism -------------------------------------------------------

    #[test]
    fn there_is_no_clock() {
        // Both of these, not just Date. `performance.now()` is the one that survives a
        // reviewer thinking "we removed Date, so there is no clock".
        let s = sandbox();
        assert_eq!(type_of(&s, "Date"), "undefined", "Date must not exist");
        assert_eq!(
            type_of(&s, "performance"),
            "undefined",
            "performance must not exist"
        );
    }

    #[test]
    fn there_is_no_randomness() {
        let s = sandbox();
        assert_eq!(
            s.eval::<String>("typeof Math.random").expect("evaluates"),
            "undefined",
            "Math.random must be gone"
        );
        assert_eq!(type_of(&s, "crypto"), "undefined", "crypto must not exist");
    }

    #[test]
    fn garbage_collection_timing_is_not_observable() {
        // WeakRef and FinalizationRegistry expose when the collector ran, which is
        // nondeterminism that does not look like a clock and so tends to be overlooked.
        let s = sandbox();
        assert_eq!(type_of(&s, "WeakRef"), "undefined");
        assert_eq!(type_of(&s, "FinalizationRegistry"), "undefined");
    }

    #[test]
    fn shared_memory_primitives_are_absent() {
        let s = sandbox();
        assert_eq!(type_of(&s, "SharedArrayBuffer"), "undefined");
        assert_eq!(type_of(&s, "Atomics"), "undefined");
    }

    #[test]
    fn the_clock_cannot_be_reached_through_a_prototype_chain() {
        // The failure mode this guards against: removing a global while leaving a path
        // back to the original through some object's constructor. Because `Date` is never
        // installed rather than deleted afterwards, there is no original to find — but
        // that is exactly the kind of claim worth checking rather than asserting.
        let s = sandbox();
        let escapes = [
            "typeof globalThis.Date",
            "typeof Object.getPrototypeOf(Object).constructor.Date",
            "typeof Reflect.get(globalThis, 'Date')",
            "typeof Object.getOwnPropertyDescriptor(globalThis, 'Date')",
            "typeof new Proxy({}, {}).Date",
        ];
        for probe in escapes {
            let result = s
                .eval::<String>(probe)
                .unwrap_or_else(|_| "undefined".to_owned());
            assert_eq!(result, "undefined", "reached a clock via: {probe}");
        }

        // And the sharpest version: constructing one via the Function intrinsic.
        // Constructing code at runtime must not find one either. An Err is equally fine:
        // it means nothing was constructed at all.
        if let Ok(kind) = s.eval::<String>("typeof (new Function('return typeof Date'))()") {
            assert_eq!(kind, "string");
        }
        let evaluated = s.eval::<String>("(new Function('return typeof Date'))()");
        if let Ok(value) = evaluated {
            assert_eq!(value, "undefined", "Function constructor reached a Date");
        }
    }

    #[test]
    fn deleting_random_does_not_break_the_rest_of_math() {
        let s = sandbox();
        assert_eq!(s.eval::<i32>("Math.max(1, 5, 3)").expect("max"), 5);
        assert_eq!(s.eval::<i32>("Math.floor(2.7)").expect("floor"), 2);
        assert_eq!(s.eval::<i32>("Math.abs(-4)").expect("abs"), 4);
    }

    // --- limits ----------------------------------------------------------------------------

    #[test]
    fn sandbox_a_rule_that_never_terminates_is_stopped() {
        let s =
            Sandbox::with_limits(Limits::default().with_rule_timeout(Duration::from_millis(120)))
                .expect("sandbox builds");

        let err = s
            .eval::<()>("while (true) {}")
            .expect_err("must be stopped");
        assert!(
            matches!(err, SandboxError::RuleTimeout { .. }),
            "expected a rule timeout, got {err:?}"
        );
        assert!(err.is_limit_breach());
    }

    #[test]
    fn sandbox_a_tight_allocation_loop_is_stopped() {
        let s = Sandbox::with_limits(
            Limits::default()
                .with_memory_bytes(2 * 1024 * 1024)
                .with_rule_timeout(Duration::from_secs(10)),
        )
        .expect("sandbox builds");

        let err = s
            .eval::<()>("const a = []; for (;;) { a.push(new Array(5000).fill(1)); }")
            .expect_err("must be stopped");

        assert!(
            matches!(err, SandboxError::MemoryExceeded { .. }),
            "expected a memory breach, got {err:?}"
        );
        assert!(err.is_limit_breach());
    }

    #[test]
    fn sandbox_the_run_budget_stops_execution_even_with_a_generous_rule_budget() {
        // The backstop case: nothing about this invocation is pathological relative to its
        // own budget, but the run is already over.
        let clock = RunClock::start(Duration::ZERO);
        let s = Sandbox::new(
            Limits::default().with_rule_timeout(Duration::from_secs(3600)),
            clock,
        )
        .expect("sandbox builds");

        let err = s
            .eval::<()>("while (true) {}")
            .expect_err("must be stopped");
        assert!(
            matches!(err, SandboxError::RunTimeout { .. }),
            "expected a run timeout, got {err:?}"
        );
    }

    #[test]
    fn sandbox_survives_a_breach_and_keeps_working() {
        // A breach cancels the run, but the sandbox itself must not be left poisoned —
        // otherwise the failure could not be reported, and tests could not assert on the
        // state afterwards.
        let s =
            Sandbox::with_limits(Limits::default().with_rule_timeout(Duration::from_millis(80)))
                .expect("sandbox builds");

        assert!(s.eval::<()>("while (true) {}").is_err());
        assert_eq!(s.eval::<i32>("1 + 1").expect("still usable"), 2);
    }

    #[test]
    fn sandbox_a_breach_is_not_reported_twice() {
        // If the trip record survived, the next invocation would be reported as timing out
        // without having run at all.
        let s =
            Sandbox::with_limits(Limits::default().with_rule_timeout(Duration::from_millis(80)))
                .expect("sandbox builds");

        assert!(matches!(
            s.eval::<()>("while (true) {}"),
            Err(SandboxError::RuleTimeout { .. })
        ));
        assert!(
            s.eval::<i32>("2 + 2").is_ok(),
            "the next invocation must start clean"
        );
    }

    #[test]
    fn sandbox_a_per_rule_budget_overrides_the_default() {
        let s =
            Sandbox::with_limits(Limits::default().with_rule_timeout(Duration::from_secs(3600)))
                .expect("sandbox builds");

        let err = s
            .eval_with_timeout::<()>("while (true) {}", Duration::from_millis(80))
            .expect_err("the explicit budget applies");
        assert!(matches!(err, SandboxError::RuleTimeout { .. }), "{err:?}");
    }

    // --- rule errors ------------------------------------------------------------------------

    #[test]
    fn a_thrown_error_is_reported_with_its_message() {
        let s = sandbox();
        let err = s
            .eval::<()>("throw new TypeError('rule blew up')")
            .expect_err("throws");

        match err {
            SandboxError::Script { message, .. } => assert_eq!(message, "rule blew up"),
            other => panic!("expected a script error, got {other:?}"),
        }
    }

    #[test]
    fn a_syntax_error_is_reported_as_a_rule_problem_not_an_engine_one() {
        let s = sandbox();
        let err = s
            .eval::<()>("this is not javascript")
            .expect_err("does not parse");
        assert!(matches!(err, SandboxError::Script { .. }), "{err:?}");
        assert!(!err.is_limit_breach());
    }

    #[test]
    fn a_thrown_error_carries_a_stack() {
        let s = sandbox();
        let err = s
            .eval::<()>(
                "function inner(){ throw new Error('deep') } function outer(){ inner() } outer()",
            )
            .expect_err("throws");

        match err {
            SandboxError::Script { stack, .. } => {
                let stack = stack.unwrap_or_default();
                assert!(
                    stack.contains("inner"),
                    "stack should name the frames: {stack}"
                );
            }
            other => panic!("expected a script error, got {other:?}"),
        }
    }
}
