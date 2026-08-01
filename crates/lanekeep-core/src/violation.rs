//! Violations, and the order they are always reported in.

use serde::{Deserialize, Serialize};

use crate::fix::Fix;
use crate::location::Location;
use crate::rule_id::RuleId;
use crate::severity::Severity;

/// One reported problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// Which rule reported it.
    pub rule_id: RuleId,
    /// Where it is.
    pub location: Location,
    /// One line saying what is wrong. Usually the rule card's message, but a rule may
    /// substitute a more specific one for a particular match.
    pub message: String,
    /// What to do about it. Usually the rule card's remediation.
    pub remediation: String,
    /// Severity as resolved by config, not as the rule declared it.
    pub severity: Severity,
    /// A replacement the rule offered, if it offered one.
    ///
    /// Not part of the canonical sort: two violations differing only in their fix are the
    /// same finding, and letting a fix change their order would make output depend on
    /// something a reader cannot see.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

/// Sort violations into lanekeep's canonical order: `(ruleId, file, line, column)`.
///
/// This is part of the output contract, not a presentation choice. An agent reads
/// lanekeep's output, changes code, and reads it again — if unrelated violations moved
/// between the two reads, the diff implies a change that did not happen. Architecture §11
/// states it plainly: repeated runs over identical input produce identical output.
///
/// The sort is total. Every field of the key is compared, so two violations can only tie
/// if they are genuinely at the same position from the same rule, and ties keep their
/// relative order via a stable sort. Nothing here depends on the order rules ran in,
/// which matters because they run in parallel and that order is not reproducible.
pub fn sort(violations: &mut [Violation]) {
    violations.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.location.file.cmp(&b.location.file))
            .then_with(|| a.location.position.line.cmp(&b.location.position.line))
            .then_with(|| a.location.position.column.cmp(&b.location.position.column))
    });
}

/// Whether any violation should fail the run.
#[must_use]
pub fn any_failing(violations: &[Violation]) -> bool {
    violations.iter().any(|v| v.severity.is_failing())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::{FilePath, Position};

    fn violation(rule: &str, file: &str, line: u32, column: u32) -> Violation {
        Violation {
            rule_id: rule.parse().expect("valid rule id"),
            location: Location::new(FilePath::new(file), Position::new(line, column)),
            message: "message".to_owned(),
            remediation: "remediation".to_owned(),
            severity: Severity::Error,
            fix: None,
        }
    }

    fn keys(violations: &[Violation]) -> Vec<String> {
        violations
            .iter()
            .map(|v| format!("{} {}", v.rule_id, v.location))
            .collect()
    }

    #[test]
    fn sorts_by_rule_then_file_then_line_then_column() {
        let mut violations = vec![
            violation("local/b", "src/a.ts", 1, 1),
            violation("lanekeep/a", "src/b.ts", 1, 1),
            violation("lanekeep/a", "src/a.ts", 2, 1),
            violation("lanekeep/a", "src/a.ts", 1, 5),
            violation("lanekeep/a", "src/a.ts", 1, 1),
        ];
        sort(&mut violations);

        assert_eq!(
            keys(&violations),
            [
                "lanekeep/a src/a.ts:1:1",
                "lanekeep/a src/a.ts:1:5",
                "lanekeep/a src/a.ts:2:1",
                "lanekeep/a src/b.ts:1:1",
                "local/b src/a.ts:1:1",
            ]
        );
    }

    #[test]
    fn line_and_column_compare_numerically() {
        // The bug this catches: comparing rendered positions as strings puts line 10
        // before line 9. It survives every small test corpus and appears the first time
        // a real file has ten lines.
        let mut violations = vec![
            violation("local/a", "src/a.ts", 10, 1),
            violation("local/a", "src/a.ts", 9, 1),
            violation("local/a", "src/a.ts", 1, 10),
            violation("local/a", "src/a.ts", 1, 9),
        ];
        sort(&mut violations);

        let positions: Vec<String> = violations
            .iter()
            .map(|v| v.location.position.to_string())
            .collect();
        assert_eq!(positions, ["1:9", "1:10", "9:1", "10:1"]);
    }

    #[test]
    fn sorting_is_independent_of_input_order() {
        // Rules run in parallel, so the order violations arrive in is not reproducible.
        // Sorting has to erase that completely or output varies run to run on unchanged
        // input.
        let canonical = {
            let mut v = vec![
                violation("lanekeep/a", "src/a.ts", 1, 1),
                violation("lanekeep/b", "src/a.ts", 1, 1),
                violation("local/a", "src/a.ts", 1, 1),
                violation("local/a", "src/b.ts", 3, 7),
            ];
            sort(&mut v);
            keys(&v)
        };

        // Every rotation of the input must produce the same output.
        let base = vec![
            violation("lanekeep/a", "src/a.ts", 1, 1),
            violation("lanekeep/b", "src/a.ts", 1, 1),
            violation("local/a", "src/a.ts", 1, 1),
            violation("local/a", "src/b.ts", 3, 7),
        ];
        for rotation in 0..base.len() {
            let mut rotated = base.clone();
            rotated.rotate_left(rotation);
            sort(&mut rotated);
            assert_eq!(
                keys(&rotated),
                canonical,
                "rotation {rotation} sorted differently"
            );
        }

        let mut reversed = base.clone();
        reversed.reverse();
        sort(&mut reversed);
        assert_eq!(keys(&reversed), canonical);
    }

    #[test]
    fn sorting_is_idempotent() {
        let mut violations = vec![
            violation("local/z", "src/z.ts", 5, 5),
            violation("lanekeep/a", "src/a.ts", 1, 1),
        ];
        sort(&mut violations);
        let once = keys(&violations);
        sort(&mut violations);
        assert_eq!(keys(&violations), once);
    }

    #[test]
    fn ties_keep_their_relative_order() {
        // Two rules can report the same position. The sort is stable, so which one is
        // listed first is at least consistent between runs rather than arbitrary.
        let mut violations = vec![
            Violation {
                message: "first".to_owned(),
                ..violation("local/a", "src/a.ts", 1, 1)
            },
            Violation {
                message: "second".to_owned(),
                ..violation("local/a", "src/a.ts", 1, 1)
            },
        ];
        sort(&mut violations);

        let messages: Vec<&str> = violations.iter().map(|v| v.message.as_str()).collect();
        assert_eq!(messages, ["first", "second"]);
    }

    #[test]
    fn handles_empty_and_single_element_input() {
        let mut empty: Vec<Violation> = Vec::new();
        sort(&mut empty);
        assert!(empty.is_empty());

        let mut one = vec![violation("local/a", "src/a.ts", 1, 1)];
        sort(&mut one);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn only_error_severity_fails_the_run() {
        let warn = Violation {
            severity: Severity::Warn,
            ..violation("local/a", "a.ts", 1, 1)
        };
        let error = violation("local/b", "a.ts", 1, 1);

        assert!(!any_failing(&[]));
        assert!(!any_failing(std::slice::from_ref(&warn)));
        assert!(any_failing(std::slice::from_ref(&error)));
        assert!(any_failing(&[warn, error]));
    }

    #[test]
    fn round_trips_through_json() {
        let original = violation("lanekeep/no-default-export", "src/a.ts", 3, 9);
        let json = serde_json::to_string(&original).expect("serializes");
        let back: Violation = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, original);
    }
}
