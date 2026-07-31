//! How much a violation matters.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A rule's configured severity.
///
/// `Off` exists as a severity rather than as a separate "disabled rules" list so that a
/// project turning a rule off, and a preset turning it back on, are the same kind of
/// operation. Config merge then has one rule to follow instead of two that can disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Reported, and does not affect the exit code.
    Warn,
    /// Reported, and makes the run exit non-zero.
    Error,
    /// Not evaluated at all. The rule is skipped before its gates are considered.
    Off,
}

impl Severity {
    /// Whether a violation at this severity should fail the run.
    #[must_use]
    pub const fn is_failing(self) -> bool {
        matches!(self, Self::Error)
    }

    /// Whether the rule runs at all.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// The severity as it appears in config and output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The string was not a severity lanekeep recognizes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown severity `{0}`: expected `error`, `warn` or `off`")]
pub struct ParseSeverityError(pub String);

impl FromStr for Severity {
    type Err = ParseSeverityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            "off" => Ok(Self::Off),
            other => Err(ParseSeverityError(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_variant() {
        assert_eq!("warn".parse(), Ok(Severity::Warn));
        assert_eq!("error".parse(), Ok(Severity::Error));
        assert_eq!("off".parse(), Ok(Severity::Off));
    }

    #[test]
    fn rejects_anything_else() {
        // Notably case-sensitive. `Error` in a config file is a typo, and accepting it
        // would mean two spellings of one value that a reader has to know are the same.
        for bad in ["Error", "ERROR", "warning", "none", "", " warn"] {
            assert!(
                bad.parse::<Severity>().is_err(),
                "should have rejected {bad:?}"
            );
        }
    }

    #[test]
    fn round_trips_through_display() {
        for severity in [Severity::Warn, Severity::Error, Severity::Off] {
            assert_eq!(severity.to_string().parse(), Ok(severity));
        }
    }

    #[test]
    fn only_error_fails_the_run() {
        assert!(Severity::Error.is_failing());
        assert!(!Severity::Warn.is_failing());
        assert!(!Severity::Off.is_failing());
    }

    #[test]
    fn only_off_disables_the_rule() {
        assert!(Severity::Error.is_enabled());
        assert!(Severity::Warn.is_enabled());
        assert!(!Severity::Off.is_enabled());
    }

    #[test]
    fn serde_uses_the_same_spelling_as_config() {
        // The JSON reporter and the config loader must agree, or a severity read from
        // config would serialize as something config cannot read back.
        assert_eq!(
            serde_json::to_string(&Severity::Error).expect("ok"),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Warn).expect("ok"),
            "\"warn\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Off).expect("ok"),
            "\"off\""
        );

        let parsed: Severity = serde_json::from_str("\"warn\"").expect("ok");
        assert_eq!(parsed, Severity::Warn);
    }
}
