//! Why component execution failed.
//!
//! The counterpart of `lanekeep_js::SandboxError`, and deliberately the same shape: the two
//! engines enforce one set of limits (`lanekeep_core::limits`), so a caller that has to tell a
//! timeout from a rule bug should not have to learn two vocabularies to do it.
//!
//! Where they differ is in what the engine hands back. QuickJS reports an interrupt as an
//! ordinary `Error` whose message happens to read "interrupted"; wasmtime reports one as a trap
//! whose rendered form begins with a backtrace. Neither is something to match on, which is why
//! `classify` reads `lanekeep_core::limits::Budget`'s own record first and touches the
//! engine's error only when nothing else explains the failure.

use std::time::Duration;

use lanekeep_core::limits::{RunClock, Trip};
use thiserror::Error;

use crate::bindings::types::{RuleError, StackFrame};

/// A failure while running a rule component.
///
/// Every variant here aborts the run. Skipping the offending rule and continuing is the
/// friendlier-looking behavior and is wrong: a timeout is timing-dependent, so a rule that
/// trips on a loaded machine and not on an idle one would make output vary between runs on
/// identical input — the property the rest of the design works to prevent. A checker that
/// could not finish must not be mistaken for one that found nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WasmError {
    /// One handler invocation exceeded its budget.
    #[error(
        "rule execution exceeded its {budget:?} budget\n  \
         the handler was still running after {budget:?} and was stopped\n  \
         a rule that cannot finish in that time usually needs a tighter query, not a \
         longer clock — raise it with `timeout` on the rule if the work is genuinely heavy"
    )]
    RuleTimeout {
        /// The budget that was exceeded.
        budget: Duration,
    },

    /// The run as a whole exceeded its wall-clock budget.
    #[error(
        "the run exceeded its {budget:?} budget after {elapsed:?}\n  \
         no single rule necessarily misbehaved — the total simply ran too long\n  \
         raise it with `--timeout`, or narrow what is being checked"
    )]
    RunTimeout {
        /// The global budget.
        budget: Duration,
        /// How long the run had actually been going.
        elapsed: Duration,
    },

    /// The store hit its memory ceiling.
    ///
    /// Summed across every linear memory in one store rather than measured per memory: a
    /// store holds one instance per component, so a per-memory ceiling would let a worker
    /// running twenty components use twenty times the budget it was given. See
    /// `crate::runtime::MemoryCeiling`.
    #[error(
        "rule execution exceeded its {limit_bytes} byte memory ceiling\n  \
         this usually means a rule accumulated without bound — check for a loop that \
         collects into an array or map that is never cleared"
    )]
    MemoryExceeded {
        /// The ceiling that was hit.
        limit_bytes: usize,
    },

    /// The guest trapped, or a host function refused the call it made.
    ///
    /// The wasm counterpart of `lanekeep_js::SandboxError::Script`. It carries no stack,
    /// because what wasmtime renders is a wasm backtrace of mangled adapter frames rather
    /// than anything a rule author wrote; the message is the root cause and nothing else.
    #[error("rule failed: {message}")]
    Guest {
        /// The trap's root cause — for a host refusal, the host's own message.
        message: String,
    },

    /// The rule caught its own failure and handed it back, rather than trapping.
    ///
    /// **The graceful half of [`WasmError::Guest`], and the two are not interchangeable.** A
    /// trap is what the host sees when a guest dies where it stands: measured 2026-08-10, an
    /// uncaught JavaScript throw inside a component arrives as
    /// `wasm trap: wasm 'unreachable' instruction executed`, with the thrown value's message,
    /// its type and its whole stack gone — because there was nowhere for any of it to go. The
    /// world's `check` and `reduce` return a `result` so that a guest whose language can catch
    /// its own failure has somewhere to put what it caught, and this variant is where that
    /// arrives.
    ///
    /// A Rust guest that panics still traps and is still [`WasmError::Guest`]. Nothing was
    /// taken away; a second, better-informed way to fail was added beside it.
    ///
    /// Rendered as `lanekeep_js::SandboxError::Script` renders a thrown error — the same
    /// wording and the same indented stack — because the two engines enforce one set of
    /// limits and report into one set of diagnostics, and a user reading a failure should not
    /// be able to tell which engine produced it from the shape of the sentence.
    ///
    /// `frames` is empty for a guest whose language has no stack to offer, which is every Rust
    /// rule, and is innermost-first for one that does. The positions are in whatever space the
    /// guest was compiled to; remapping them through a component's source map happens above
    /// this type, not in it.
    #[error("rule threw: {message}{}", indent_frames(.frames))]
    RuleFailed {
        /// The guest's own message.
        message: String,
        /// The guest's own stack, innermost first.
        frames: Vec<StackFrame>,
    },

    /// A component refused the options it was configured with.
    ///
    /// **Distinct from [`WasmError::Guest`] on purpose, and not a second spelling of it.**
    /// Both carry a string the guest wrote, which is what makes them easy to conflate — but a
    /// trap means `configure` could not finish (it crashed, or a host call inside it was
    /// refused), while this means `configure` finished and declined on purpose. `configure`'s
    /// own doc in `wit/world.wit` says why the difference is worth a caller's attention: "a
    /// rule that cannot use its configuration has not merely misbehaved, it has been
    /// misconfigured, and the user needs to be told which." Reusing [`WasmError::Guest`] would
    /// erase exactly that distinction.
    ///
    /// **The message alone cannot prove the two are distinguished, and asserting on it alone
    /// so nearly did.** [`WasmError::Guest`]'s own `Display` is `"rule failed: {message}"`,
    /// which contains any guest-written text this variant's does too — so a test asserting
    /// only `format!("{error}").contains(...)` passes whichever variant `configure` maps a
    /// refusal onto, and proves nothing about the mapping.
    /// `tests/metadata.rs::a_guest_that_refuses_its_options_fails_the_call_with_its_own_message`
    /// asserts `matches!(error, WasmError::Misconfigured { .. })` for exactly this reason, kept
    /// beside the message assertion rather than instead of it.
    #[error("rule refused its configured options: {message}")]
    Misconfigured {
        /// The guest's own refusal message.
        message: String,
    },

    /// A component reaches for capability the sandbox does not grant.
    ///
    /// Read off the artifact's import list before it is instantiated, and a variant of its
    /// own rather than an [`WasmError::Engine`] string because it is the one failure here
    /// that is about *authority* rather than about a broken build. See `crate::load`.
    ///
    /// **A rejection here is not the same as a failure to instantiate**, even though today
    /// an offending component would also fail to instantiate. That protection is a
    /// consequence of this host declining to link WASI, and it disappears the moment any
    /// host links WASI for any reason; this one does not.
    #[error(
        "rule component `{name}` imports capability the sandbox does not grant: {}\n  \
         permitted: {}\n  \
         the usual cause is a component built for `wasm32-wasip1` rather than \
         `wasm32-unknown-unknown` — `cargo component` defaults to the former, whose \
         adapter imports a wall clock and a filesystem as soon as the guest touches `std`",
        .imports.join(", "),
        if .permitted.is_empty() { "nothing".to_owned() } else { .permitted.join(", ") }
    )]
    ForbiddenImports {
        /// Whatever identifies the artifact to a reader — a rule id, or a path.
        name: String,
        /// The imports that were not permitted, sorted.
        imports: Vec<String>,
        /// The set that was permitted, sorted.
        permitted: Vec<String>,
    },

    /// A component described a rule the host will not run.
    ///
    /// Read off the `metadata` export at prepare time, and refused rather than run: a rule
    /// whose metadata names no language, or declares a conjunctive content gate, would load and
    /// report nothing — which is indistinguishable from the code being clean. This is the
    /// component half of the checks `lanekeep-config` runs on a TypeScript rule's default export.
    #[error("rule `{rule}` has invalid metadata: {detail}")]
    InvalidMetadata {
        /// The rule's own id, so the refusal names what to fix.
        rule: String,
        /// What about the metadata is wrong.
        detail: String,
    },

    /// The runtime failed for a reason lanekeep does not model.
    ///
    /// Building an engine, compiling a component, linking the world, instantiating: the
    /// failures that mean a broken build or a malformed artifact rather than anything about
    /// how a rule behaved.
    #[error("webassembly runtime error: {0}")]
    Engine(String),
}

impl WasmError {
    /// Whether this failure came from a breached limit rather than from rule logic.
    ///
    /// Both cancel the run. The distinction is for the diagnostic: a limit breach is a
    /// budget problem, a trap is a bug in the rule.
    #[must_use]
    pub const fn is_limit_breach(&self) -> bool {
        matches!(
            self,
            Self::RuleTimeout { .. } | Self::RunTimeout { .. } | Self::MemoryExceeded { .. }
        )
    }
}

impl From<RuleError> for WasmError {
    /// A guest's own account of why it failed, unchanged.
    ///
    /// A `From` rather than a constructor because there is nothing to decide: unlike
    /// `classify` below, which weighs a recorded [`Trip`] and a memory breach against what the
    /// engine said, this failure arrived as a value the guest built on purpose. Anything the
    /// host added here would be the host's opinion of a rule's own report.
    fn from(error: RuleError) -> Self {
        Self::RuleFailed {
            message: error.message,
            frames: error.frames,
        }
    }
}

/// The guest's stack, one frame to a line, indented under the message.
///
/// The same arrangement `lanekeep_js::error::indent_stack` produces for a thrown JavaScript
/// error, and spelled `function@file:line:column` because that is the form QuickJS already
/// hands back — `boom@throw.js:13:25`. Two engines rendering one concept two ways is a
/// difference a reader would have to learn for nothing.
fn indent_frames(frames: &[StackFrame]) -> String {
    use std::fmt::Write as _;

    frames.iter().fold(String::new(), |mut out, frame| {
        // Writing into a String cannot fail, and swallowing the Result here keeps this a
        // plain fold rather than a loop with an unreachable error arm.
        let _ = write!(
            out,
            "\n  {}@{}:{}:{}",
            frame.function, frame.file, frame.line, frame.column
        );
        out
    })
}

/// Turn a raw wasmtime failure into something that says what actually happened.
///
/// Three branches, in this order, mirroring `lanekeep_js::Sandbox::classify`:
///
/// 1. **The recorded [`Trip`].** Our own record, taken before anything the engine said is
///    read. wasmtime renders an epoch trap with a backtrace whose top frame is spelled
///    `wit-component:shim!indirect-...`, and matching on that would make the difference
///    between "your rule looped forever" and "your rule threw" depend on wording this
///    project does not control.
/// 2. **The memory ceiling.** A distinct path rather than a second kind of trip, because a
///    growth refusal never goes through the epoch mechanism at all: it is
///    `wasmtime::ResourceLimiter::memory_growing` returning an error, which traps the guest
///    directly. A `Trip` is what the epoch callback records, and there is none here.
/// 3. **The guest's own failure.** `root_cause` and not the rendered error. wasmtime prefixes
///    a host function's error with a backtrace that already names the method, so a test
///    asserting on `format!("{err:?}")` passes whatever the host said — that is a trap this
///    repository has already been caught by. The root cause is the host's message alone.
///
/// The three inputs after `error` are read by the caller rather than by this function, so it
/// can be exercised without a store, a component and a live epoch thread.
pub(crate) fn classify(
    error: &wasmtime::Error,
    trip: Option<Trip>,
    clock: &RunClock,
    timeout: Duration,
    memory_breach: Option<usize>,
) -> WasmError {
    match trip {
        Some(Trip::Run) => {
            return WasmError::RunTimeout {
                budget: clock.global_timeout(),
                elapsed: clock.elapsed(),
            };
        }
        Some(Trip::Rule) => return WasmError::RuleTimeout { budget: timeout },
        None => {}
    }

    if let Some(limit_bytes) = memory_breach {
        return WasmError::MemoryExceeded { limit_bytes };
    }

    WasmError::Guest {
        message: error.root_cause().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn clock() -> Arc<RunClock> {
        RunClock::start(Duration::from_secs(15))
    }

    fn boom() -> wasmtime::Error {
        wasmtime::Error::msg("the guest went wrong")
    }

    #[test]
    fn limit_breaches_are_distinguishable_from_rule_bugs() {
        assert!(
            WasmError::RuleTimeout {
                budget: Duration::from_secs(1)
            }
            .is_limit_breach()
        );
        assert!(
            WasmError::RunTimeout {
                budget: Duration::from_secs(15),
                elapsed: Duration::from_secs(16),
            }
            .is_limit_breach()
        );
        assert!(WasmError::MemoryExceeded { limit_bytes: 1024 }.is_limit_breach());

        assert!(
            !WasmError::Guest {
                message: "boom".to_owned()
            }
            .is_limit_breach()
        );
        assert!(!WasmError::Engine("odd".to_owned()).is_limit_breach());
        assert!(
            !WasmError::ForbiddenImports {
                name: "example".to_owned(),
                imports: vec!["wasi:clocks/wall-clock@0.2.3".to_owned()],
                permitted: Vec::new(),
            }
            .is_limit_breach()
        );
        assert!(
            !WasmError::Misconfigured {
                message: "expected an object".to_owned()
            }
            .is_limit_breach(),
            "a refusal is a configuration problem, not a budget problem"
        );
    }

    #[test]
    fn a_forbidden_import_is_named_and_so_is_the_usual_cause() {
        // The whole value of this variant over `Engine(String)` is that a reader learns
        // which import and what to do about it. A message that named neither would still
        // fail the run and would tell nobody why.
        let rendered = WasmError::ForbiddenImports {
            name: "acme/no-console".to_owned(),
            imports: vec![
                "wasi:clocks/wall-clock@0.2.3".to_owned(),
                "wasi:filesystem/types@0.2.3".to_owned(),
            ],
            permitted: vec!["lanekeep:host/types@0.1.0".to_owned()],
        }
        .to_string();
        assert!(rendered.contains("acme/no-console"), "{rendered}");
        assert!(
            rendered.contains("wasi:clocks/wall-clock@0.2.3"),
            "{rendered}"
        );
        assert!(
            rendered.contains("wasi:filesystem/types@0.2.3"),
            "{rendered}"
        );
        assert!(rendered.contains("lanekeep:host/types@0.1.0"), "{rendered}");
        assert!(rendered.contains("wasm32-wasip1"), "{rendered}");
    }

    #[test]
    fn an_empty_permitted_set_reads_as_nothing_rather_than_as_a_blank() {
        // `permitted: ` with an empty line after it reads as a formatting bug rather than
        // as the deliberate answer it is.
        let rendered = WasmError::ForbiddenImports {
            name: "example".to_owned(),
            imports: vec!["lanekeep:host/types@0.1.0".to_owned()],
            permitted: Vec::new(),
        }
        .to_string();
        assert!(rendered.contains("permitted: nothing"), "{rendered}");
    }

    #[test]
    fn a_misconfigured_rules_own_message_survives_into_the_rendering() {
        // The whole value of this variant over `Guest` is that a reader can tell "the guest
        // declined" from "the guest crashed" without parsing prose — but both carry a string
        // the guest wrote, so the rendering has to carry it through rather than replace it
        // with a generic word for the case.
        let rendered = WasmError::Misconfigured {
            message: "expected an object".to_owned(),
        }
        .to_string();
        assert!(rendered.contains("expected an object"), "{rendered}");
    }

    #[test]
    fn a_recorded_trip_outranks_whatever_the_engine_said() {
        // The whole reason `Budget` records a trip. The error handed in here is an ordinary
        // failure with no hint of a timeout in it, and the classification is still a timeout,
        // because our own record is the authority.
        let clock = clock();
        let rule = classify(
            &boom(),
            Some(Trip::Rule),
            &clock,
            Duration::from_millis(80),
            None,
        );
        assert_eq!(
            rule,
            WasmError::RuleTimeout {
                budget: Duration::from_millis(80)
            }
        );

        let run = classify(
            &boom(),
            Some(Trip::Run),
            &clock,
            Duration::from_secs(1),
            None,
        );
        assert!(
            matches!(run, WasmError::RunTimeout { budget, .. } if budget == Duration::from_secs(15)),
            "{run:?}"
        );
    }

    #[test]
    fn a_trip_outranks_a_memory_breach_recorded_in_the_same_call() {
        // Both can be true at once: a guest stopped mid-allocation records a trip, and the
        // allocation that was in flight may already have been refused. The trip is the more
        // consequential fact, on the same reasoning `Budget` uses to prefer the run over the
        // rule when both are spent.
        let clock = clock();
        let classified = classify(
            &boom(),
            Some(Trip::Rule),
            &clock,
            Duration::from_millis(80),
            Some(4096),
        );
        assert_eq!(
            classified,
            WasmError::RuleTimeout {
                budget: Duration::from_millis(80)
            }
        );
    }

    #[test]
    fn a_memory_breach_is_read_from_the_limiter_and_not_from_the_trap_text() {
        let clock = clock();
        let classified = classify(&boom(), None, &clock, Duration::from_secs(1), Some(4096));
        assert_eq!(classified, WasmError::MemoryExceeded { limit_bytes: 4096 });
    }

    #[test]
    fn an_unexplained_failure_carries_the_root_cause_and_not_the_rendering() {
        // `AGENTS.md`: a wasm trap's rendered error already contains the method name, so
        // asserting on `format!("{err:?}")` proves nothing. What this variant carries is the
        // root cause, which for a host refusal is the host's own message and nothing else.
        let clock = clock();
        let context = boom().context("while calling check-context.report");
        let classified = classify(&context, None, &clock, Duration::from_secs(1), None);
        assert_eq!(
            classified,
            WasmError::Guest {
                message: "the guest went wrong".to_owned()
            },
            "the context wrapper must not become the message"
        );
    }

    #[test]
    fn the_rule_timeout_message_suggests_the_usual_fix() {
        let rendered = WasmError::RuleTimeout {
            budget: Duration::from_secs(1),
        }
        .to_string();
        assert!(rendered.contains("tighter query"), "{rendered}");
    }

    #[test]
    fn the_run_timeout_message_does_not_blame_a_rule() {
        let rendered = WasmError::RunTimeout {
            budget: Duration::from_secs(15),
            elapsed: Duration::from_secs(16),
        }
        .to_string();
        assert!(rendered.contains("no single rule"), "{rendered}");
    }
}
