//! Why sandboxed execution failed.

use std::time::Duration;

use thiserror::Error;

/// A failure inside the sandbox.
///
/// Every variant here aborts the run. Skipping the offending rule and continuing is the
/// friendlier-looking behavior and is wrong: a timeout is timing-dependent, so a rule that
/// trips on a loaded machine and not on an idle one would make output vary between runs on
/// identical input — the property the rest of the design works to prevent. A checker that
/// could not finish must not be mistaken for one that found nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SandboxError {
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

    /// The runtime hit its memory ceiling.
    #[error(
        "rule execution exceeded its {limit_bytes} byte memory ceiling\n  \
         this usually means a rule accumulated without bound — check for a loop that \
         collects into an array or map that is never cleared"
    )]
    MemoryExceeded {
        /// The ceiling that was hit.
        limit_bytes: usize,
    },

    /// The rule threw, or failed to parse.
    #[error("rule threw: {message}{}", crate::error::indent_stack(.stack.as_deref()))]
    Script {
        /// The thrown value's message.
        message: String,
        /// The stack trace, if the thrown value carried one.
        stack: Option<String>,
    },

    /// The rule threw something that is not an `Error`.
    #[error(
        "rule threw a non-Error value\n  \
         throwing a string or object loses the stack trace; throw an Error instead"
    )]
    NonErrorThrown,

    /// The engine failed for a reason lanekeep does not model.
    #[error("javascript engine error: {0}")]
    Engine(String),
}

impl SandboxError {
    /// Whether this failure came from a breached limit rather than from rule logic.
    ///
    /// Both cancel the run. The distinction is for the diagnostic: a limit breach is a
    /// budget problem, a thrown error is a bug in the rule.
    #[must_use]
    pub const fn is_limit_breach(&self) -> bool {
        matches!(
            self,
            Self::RuleTimeout { .. } | Self::RunTimeout { .. } | Self::MemoryExceeded { .. }
        )
    }
}

fn indent_stack(stack: Option<&str>) -> String {
    use std::fmt::Write as _;

    match stack {
        Some(s) if !s.trim().is_empty() => s.lines().fold(String::new(), |mut out, line| {
            // Writing into a String cannot fail, and swallowing the Result here keeps this
            // a plain fold rather than a loop with an unreachable error arm.
            let _ = write!(out, "\n  {}", line.trim_end());
            out
        }),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_breaches_are_distinguishable_from_rule_bugs() {
        assert!(
            SandboxError::RuleTimeout {
                budget: Duration::from_secs(1)
            }
            .is_limit_breach()
        );
        assert!(
            SandboxError::RunTimeout {
                budget: Duration::from_secs(15),
                elapsed: Duration::from_secs(16),
            }
            .is_limit_breach()
        );
        assert!(SandboxError::MemoryExceeded { limit_bytes: 1024 }.is_limit_breach());

        assert!(
            !SandboxError::Script {
                message: "boom".to_owned(),
                stack: None
            }
            .is_limit_breach()
        );
        assert!(!SandboxError::NonErrorThrown.is_limit_breach());
        assert!(!SandboxError::Engine("odd".to_owned()).is_limit_breach());
    }

    #[test]
    fn the_rule_timeout_message_suggests_the_usual_fix() {
        // The common cause is a query so broad the handler runs on thousands of matches,
        // and the fix is the query rather than the clock. Saying so is most of the value.
        let rendered = SandboxError::RuleTimeout {
            budget: Duration::from_secs(1),
        }
        .to_string();
        assert!(rendered.contains("tighter query"), "{rendered}");
        assert!(rendered.contains("timeout"), "{rendered}");
    }

    #[test]
    fn the_run_timeout_message_does_not_blame_a_rule() {
        // When the global budget fires, no individual rule necessarily misbehaved. A
        // message implying otherwise sends the reader looking for a culprit that is not
        // there.
        let rendered = SandboxError::RunTimeout {
            budget: Duration::from_secs(15),
            elapsed: Duration::from_secs(16),
        }
        .to_string();
        assert!(rendered.contains("no single rule"), "{rendered}");
    }

    #[test]
    fn a_script_error_renders_its_stack_indented() {
        let rendered = SandboxError::Script {
            message: "cannot read x".to_owned(),
            stack: Some("at check (rule.ts:4:2)\nat <eval> (rule.ts:1:1)".to_owned()),
        }
        .to_string();

        assert!(
            rendered.starts_with("rule threw: cannot read x"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\n  at check (rule.ts:4:2)"),
            "{rendered}"
        );
    }

    #[test]
    fn a_script_error_without_a_stack_renders_cleanly() {
        let rendered = SandboxError::Script {
            message: "boom".to_owned(),
            stack: None,
        }
        .to_string();
        assert_eq!(rendered, "rule threw: boom");

        let blank = SandboxError::Script {
            message: "boom".to_owned(),
            stack: Some("  \n".to_owned()),
        }
        .to_string();
        assert_eq!(
            blank, "rule threw: boom",
            "whitespace-only stack should not add lines"
        );
    }
}
