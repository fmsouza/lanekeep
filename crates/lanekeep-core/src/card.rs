//! The rule card.
//!
//! `message`, `remediation` and `examples` are mandatory fields on every rule. They are
//! not documentation. They are the payload `lanekeep explain` prints, the agent reporter
//! emits, and context injection feeds to a model so it learns the rule *before* generating
//! rather than after.
//!
//! A rule whose card says only "this is not allowed" tells an agent nothing it can act on,
//! and the whole premise of the tool is that the feedback loop closes.

use serde::{Deserialize, Serialize};

/// A matched pair showing the rule's point better than prose does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Examples {
    /// Code the rule reports.
    pub bad: String,
    /// The corresponding code it does not.
    pub good: String,
}

/// Everything a reader — human or model — needs to act on a violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCard {
    /// One line saying what is wrong. Not what to do about it.
    pub message: String,
    /// What to do instead. Specific enough to act on without reading the rule source.
    pub remediation: String,
    /// A bad/good pair.
    pub examples: Examples,
}

/// Why a card is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardProblem {
    /// `message` is empty or whitespace.
    EmptyMessage,
    /// `remediation` is empty or whitespace.
    EmptyRemediation,
    /// An example is empty or whitespace.
    EmptyExample,
    /// The bad and good examples are the same, so the pair demonstrates nothing.
    IdenticalExamples,
}

impl RuleCard {
    /// Check that the card can actually do its job.
    ///
    /// This is deliberately more than a null check. A card is the tool's output, and an
    /// empty remediation produces a violation a reader cannot act on — which is
    /// indistinguishable, from their side, from lanekeep being wrong.
    ///
    /// # Errors
    ///
    /// Returns every problem found, so a rule author fixes them in one pass rather than
    /// one per run.
    pub fn validate(&self) -> Result<(), Vec<CardProblem>> {
        let mut problems = Vec::new();

        if self.message.trim().is_empty() {
            problems.push(CardProblem::EmptyMessage);
        }
        if self.remediation.trim().is_empty() {
            problems.push(CardProblem::EmptyRemediation);
        }
        if self.examples.bad.trim().is_empty() || self.examples.good.trim().is_empty() {
            problems.push(CardProblem::EmptyExample);
        } else if self.examples.bad.trim() == self.examples.good.trim() {
            problems.push(CardProblem::IdenticalExamples);
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(message: &str, remediation: &str, bad: &str, good: &str) -> RuleCard {
        RuleCard {
            message: message.to_owned(),
            remediation: remediation.to_owned(),
            examples: Examples {
                bad: bad.to_owned(),
                good: good.to_owned(),
            },
        }
    }

    fn valid() -> RuleCard {
        card(
            "Literal numeric size inside makeStyles",
            "Use theme.spacing.* instead",
            "padding: 12",
            "padding: theme.spacing.md",
        )
    }

    #[test]
    fn accepts_a_complete_card() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn rejects_an_empty_message() {
        let mut c = valid();
        c.message = String::new();
        assert_eq!(c.validate(), Err(vec![CardProblem::EmptyMessage]));
    }

    #[test]
    fn rejects_an_empty_remediation() {
        let mut c = valid();
        c.remediation = String::new();
        assert_eq!(c.validate(), Err(vec![CardProblem::EmptyRemediation]));
    }

    #[test]
    fn treats_whitespace_as_empty() {
        // A card with `remediation: "   "` passes a null check and fails a reader.
        let c = card("  ", "\t\n", " ", "  ");
        let problems = c.validate().expect_err("should reject");
        assert!(problems.contains(&CardProblem::EmptyMessage));
        assert!(problems.contains(&CardProblem::EmptyRemediation));
        assert!(problems.contains(&CardProblem::EmptyExample));
    }

    #[test]
    fn rejects_examples_that_demonstrate_nothing() {
        let c = card("msg", "fix", "padding: 12", "padding: 12");
        assert_eq!(c.validate(), Err(vec![CardProblem::IdenticalExamples]));
    }

    #[test]
    fn compares_examples_ignoring_surrounding_whitespace() {
        // Copy-paste into a YAML block or a template literal picks up indentation, and
        // "identical apart from leading spaces" is still a pair that shows nothing.
        let c = card("msg", "fix", "  padding: 12  ", "padding: 12");
        assert_eq!(c.validate(), Err(vec![CardProblem::IdenticalExamples]));
    }

    #[test]
    fn reports_every_problem_at_once() {
        // One problem per run would make fixing a bad card an N-round-trip exercise.
        let c = card("", "", "", "");
        let problems = c.validate().expect_err("should reject");
        assert_eq!(problems.len(), 3);
    }

    #[test]
    fn empty_examples_are_reported_instead_of_identical() {
        // Both conditions hold when the examples are two empty strings. Reporting
        // "identical" there would be technically true and useless.
        let c = card("msg", "fix", "", "");
        assert_eq!(c.validate(), Err(vec![CardProblem::EmptyExample]));
    }

    #[test]
    fn round_trips_through_json() {
        let c = valid();
        let json = serde_json::to_string(&c).expect("serializes");
        let back: RuleCard = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, c);
    }
}
