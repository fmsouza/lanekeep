//! Output formatting for lanekeep.
//!
//! The human and JSON reporters. SARIF and the agent reporter arrive in M2.
//!
//! Violations are always sorted by `(ruleId, file, line, column)` before they reach a
//! reporter. Determinism matters more than usual here: an agent reads this output twice
//! and must not see reordering as change.

use std::fmt::Write as _;

use lanekeep_core::{Severity, Violation};
use serde::Serialize;

/// Which format to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable, the default.
    Human,
    /// Machine-readable, versioned.
    Json,
}

impl Format {
    /// Parse a `--format` value.
    ///
    /// # Errors
    ///
    /// Returns the unrecognized value, so the caller can name it in a diagnostic.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            other => Err(other.to_owned()),
        }
    }
}

/// Whether to emit ANSI color.
///
/// Resolved by the caller rather than probed here, so a reporter is a pure function of its
/// inputs and a test does not depend on where its output happens to be going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Emit escape sequences.
    Always,
    /// Emit none.
    Never,
}

impl Color {
    /// The usual rule: color only for a terminal, and never when `NO_COLOR` is set.
    ///
    /// `NO_COLOR` is honored for any non-empty value, per the convention.
    #[must_use]
    pub fn resolve(is_terminal: bool, no_color_env: Option<&str>) -> Self {
        if no_color_env.is_some_and(|v| !v.is_empty()) || !is_terminal {
            Self::Never
        } else {
            Self::Always
        }
    }

    const fn paint<'a>(self, code: &'static str, text: &'a str) -> Painted<'a> {
        Painted {
            code,
            text,
            enabled: matches!(self, Self::Always),
        }
    }
}

struct Painted<'a> {
    code: &'static str,
    text: &'a str,
    enabled: bool,
}

impl std::fmt::Display for Painted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.enabled {
            write!(f, "\x1b[{}m{}\x1b[0m", self.code, self.text)
        } else {
            f.write_str(self.text)
        }
    }
}

const RED: &str = "31";
const YELLOW: &str = "33";
const GREEN: &str = "32";
const DIM: &str = "2";

/// What a run produced, as a reporter sees it.
#[derive(Debug, Clone, Copy)]
pub struct Summary {
    /// Files discovery selected.
    pub files_discovered: usize,
    /// Files actually parsed, after gates.
    pub files_parsed: usize,
    /// Whether `--warn-only` was passed.
    pub warn_only: bool,
}

/// Render violations.
#[must_use]
pub fn render(format: Format, color: Color, violations: &[Violation], summary: Summary) -> String {
    match format {
        Format::Human => human(color, violations, summary),
        Format::Json => json(violations, summary),
    }
}

fn human(color: Color, violations: &[Violation], summary: Summary) -> String {
    if violations.is_empty() {
        return format!(
            "{}\n",
            color.paint(
                GREEN,
                &format!("✔ no violations in {} file(s)", summary.files_discovered)
            )
        );
    }

    let mut out = String::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;

    for violation in violations {
        let (code, label) = match violation.severity {
            Severity::Error => {
                errors += 1;
                (RED, "error")
            }
            Severity::Warn => {
                warnings += 1;
                (YELLOW, "warn")
            }
            // A violation from a disabled rule cannot occur — the engine drops those
            // rules before running. Rendering it as a warning is the harmless reading if
            // it ever does.
            Severity::Off => (YELLOW, "warn"),
        };

        let _ = writeln!(
            out,
            "{} {} {} {}",
            color.paint(code, &violation.location.to_string()),
            color.paint(code, label),
            color.paint(DIM, &format!("[{}]", violation.rule_id)),
            violation.message,
        );
        let _ = writeln!(out, "  {} {}", color.paint(DIM, "→"), violation.remediation);
    }

    let counts = match (errors, warnings) {
        (e, 0) => format!("{e} error(s)"),
        (0, w) => format!("{w} warning(s)"),
        (e, w) => format!("{e} error(s), {w} warning(s)"),
    };
    let marker = if errors > 0 && !summary.warn_only {
        "✖"
    } else {
        "⚠"
    };
    let code = if errors > 0 && !summary.warn_only {
        RED
    } else {
        YELLOW
    };
    let suffix = if summary.warn_only {
        " — warn-only, not blocking"
    } else {
        ""
    };

    let _ = writeln!(
        out,
        "\n{}",
        color.paint(
            code,
            &format!(
                "{marker} {counts} across {} file(s) checked{suffix}",
                summary.files_parsed
            )
        )
    );

    out
}

/// The JSON payload. A versioned, stable schema — anything consuming this is entitled to
/// rely on it, so the version moves whenever the shape does.
#[derive(Debug, Serialize)]
struct Payload<'a> {
    /// Schema version, incremented on any shape change.
    version: u32,
    ok: bool,
    total: usize,
    files_discovered: usize,
    files_parsed: usize,
    warn_only: bool,
    violations: &'a [Violation],
}

fn json(violations: &[Violation], summary: Summary) -> String {
    let payload = Payload {
        version: 1,
        ok: violations.is_empty(),
        total: violations.len(),
        files_discovered: summary.files_discovered,
        files_parsed: summary.files_parsed,
        warn_only: summary.warn_only,
        violations,
    };

    serde_json::to_string_pretty(&payload).map_or_else(
        // Serializing a plain struct cannot fail. Emitting a valid empty document rather
        // than panicking keeps a formatting bug from looking like a crash in the checker.
        |_| "{\"version\":1,\"ok\":false,\"total\":0,\"violations\":[]}\n".to_owned(),
        |mut rendered| {
            rendered.push('\n');
            rendered
        },
    )
}

/// The exit code for a run, per §11.
#[must_use]
pub fn exit_code(violations: &[Violation], warn_only: bool) -> i32 {
    i32::from(!warn_only && violations.iter().any(|v| v.severity == Severity::Error))
}

#[cfg(test)]
mod tests {
    use lanekeep_core::{FilePath, Location, Position, RuleId};

    use super::*;

    fn violation(rule: &str, file: &str, line: u32, severity: Severity) -> Violation {
        Violation {
            rule_id: rule.parse::<RuleId>().expect("valid"),
            location: Location::new(FilePath::new(file), Position::new(line, 1)),
            message: "something is wrong".to_owned(),
            remediation: "do it the other way".to_owned(),
            severity,
        }
    }

    fn summary() -> Summary {
        Summary {
            files_discovered: 3,
            files_parsed: 2,
            warn_only: false,
        }
    }

    #[test]
    fn human_output_is_plain_without_color() {
        let out = render(
            Format::Human,
            Color::Never,
            &[violation("local/a", "src/a.ts", 4, Severity::Error)],
            summary(),
        );
        assert!(
            !out.contains('\x1b'),
            "no escapes when color is off: {out:?}"
        );
        assert!(out.contains("src/a.ts:4:1"), "{out}");
        assert!(out.contains("[local/a]"), "{out}");
        assert!(out.contains("something is wrong"), "{out}");
        assert!(out.contains("do it the other way"), "{out}");
    }

    #[test]
    fn human_output_colors_when_asked() {
        let out = render(
            Format::Human,
            Color::Always,
            &[violation("local/a", "src/a.ts", 4, Severity::Error)],
            summary(),
        );
        assert!(out.contains('\x1b'), "should contain escapes");
    }

    #[test]
    fn a_clean_run_says_so() {
        let out = render(Format::Human, Color::Never, &[], summary());
        assert!(out.contains("no violations"), "{out}");
        assert!(out.contains('3'), "should say how many files: {out}");
    }

    #[test]
    fn the_footer_counts_each_severity() {
        let out = render(
            Format::Human,
            Color::Never,
            &[
                violation("local/a", "a.ts", 1, Severity::Error),
                violation("local/b", "b.ts", 1, Severity::Warn),
                violation("local/c", "c.ts", 1, Severity::Warn),
            ],
            summary(),
        );
        assert!(out.contains("1 error(s), 2 warning(s)"), "{out}");
    }

    #[test]
    fn warn_only_changes_the_marker_and_says_why() {
        let out = render(
            Format::Human,
            Color::Never,
            &[violation("local/a", "a.ts", 1, Severity::Error)],
            Summary {
                warn_only: true,
                ..summary()
            },
        );
        assert!(out.contains("warn-only"), "{out}");
        assert!(!out.contains('✖'), "should not look like a failure: {out}");
    }

    #[test]
    fn json_is_valid_and_versioned() {
        let out = render(
            Format::Json,
            Color::Never,
            &[violation("local/a", "src/a.ts", 4, Severity::Error)],
            summary(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");

        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["files_discovered"], 3);
        assert_eq!(parsed["violations"][0]["rule_id"], "local/a");
        assert_eq!(parsed["violations"][0]["location"]["file"], "src/a.ts");
        assert_eq!(parsed["violations"][0]["location"]["position"]["line"], 4);
        assert_eq!(parsed["violations"][0]["severity"], "error");
    }

    #[test]
    fn json_never_emits_color() {
        // A machine consumer receiving escape sequences would have to strip them, and the
        // format is not the place to depend on where output is going.
        let out = render(
            Format::Json,
            Color::Always,
            &[violation("local/a", "a.ts", 1, Severity::Error)],
            summary(),
        );
        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn json_for_a_clean_run_is_still_a_full_document() {
        let out = render(Format::Json, Color::Never, &[], summary());
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["total"], 0);
        assert!(parsed["violations"].as_array().is_some_and(Vec::is_empty));
    }

    #[test]
    fn output_is_byte_identical_for_identical_input() {
        // The property an agent depends on. Rendering twice must not differ.
        let violations = [
            violation("local/b", "b.ts", 2, Severity::Warn),
            violation("local/a", "a.ts", 1, Severity::Error),
        ];
        for format in [Format::Human, Format::Json] {
            let first = render(format, Color::Never, &violations, summary());
            for _ in 0..5 {
                assert_eq!(render(format, Color::Never, &violations, summary()), first);
            }
        }
    }

    #[test]
    fn color_resolution_follows_the_convention() {
        assert_eq!(Color::resolve(true, None), Color::Always);
        assert_eq!(Color::resolve(false, None), Color::Never, "not a terminal");
        assert_eq!(
            Color::resolve(true, Some("1")),
            Color::Never,
            "NO_COLOR set"
        );
        assert_eq!(
            Color::resolve(true, Some("")),
            Color::Always,
            "an empty NO_COLOR does not count, per the convention"
        );
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        let error = [violation("local/a", "a.ts", 1, Severity::Error)];
        let warn = [violation("local/a", "a.ts", 1, Severity::Warn)];

        assert_eq!(exit_code(&[], false), 0, "clean");
        assert_eq!(exit_code(&warn, false), 0, "warnings do not fail");
        assert_eq!(exit_code(&error, false), 1, "errors fail");
        assert_eq!(
            exit_code(&error, true),
            0,
            "warn-only suppresses the failure"
        );
    }

    #[test]
    fn format_parsing_names_what_it_did_not_recognize() {
        assert_eq!(Format::parse("human"), Ok(Format::Human));
        assert_eq!(Format::parse("json"), Ok(Format::Json));
        assert_eq!(Format::parse("sarif"), Err("sarif".to_owned()));
    }
}
