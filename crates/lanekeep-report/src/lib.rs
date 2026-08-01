//! Output formatting for lanekeep.
//!
//! Four reporters: `human`, `json`, `sarif` and `agent`.
//!
//! Violations are always sorted by `(ruleId, file, line, column)` before they reach a
//! reporter. Determinism matters more than usual here: an agent reads this output twice
//! and must not see reordering as change.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lanekeep_core::{RuleCard, RuleId, Severity, Violation};
use serde::Serialize;

/// Which format to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable, the default.
    Human,
    /// Machine-readable, versioned.
    Json,
    /// SARIF 2.1.0, for GitHub code scanning.
    Sarif,
    /// Token-minimal and remediation-first, for an agent.
    Agent,
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
            "sarif" => Ok(Self::Sarif),
            "agent" => Ok(Self::Agent),
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
pub fn render(
    format: Format,
    color: Color,
    violations: &[Violation],
    summary: Summary,
    cards: &Cards,
) -> String {
    match format {
        Format::Human => human(color, violations, summary),
        Format::Json => json(violations, summary),
        Format::Sarif => sarif(violations, cards),
        Format::Agent => agent(violations, cards),
    }
}

/// The rule cards a run had available, by rule id.
///
/// Only the agent and SARIF reporters use them: both describe a rule as well as its
/// violations, and a `Violation` carries the message and remediation but not the examples.
/// Passed in rather than looked up, so a reporter stays a pure function of its inputs.
pub type Cards = BTreeMap<RuleId, RuleCard>;

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

/// SARIF 2.1.0.
///
/// The format GitHub code scanning ingests, which is the whole reason it exists here — a
/// project already paying for lanekeep in CI gets annotations on the diff for free.
///
/// Built with `serde_json::json!` rather than a typed model of the specification. SARIF is
/// large and mostly optional, this emits the handful of required properties plus what
/// GitHub actually reads, and a full model would be a great deal of code standing between
/// this and a format that would still need checking against the consumer.
fn sarif(violations: &[Violation], cards: &Cards) -> String {
    // Rules are described once and referenced by index, which is what the format wants and
    // also what keeps the document from repeating a card per violation. Only rules that
    // actually fired: a listing of every configured rule would be noise in a diff view.
    let fired: Vec<&RuleId> = violations
        .iter()
        .map(|v| &v.rule_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let rules: Vec<serde_json::Value> = fired
        .iter()
        .map(|id| {
            let card = cards.get(*id);
            let message = card.map_or_else(|| id.to_string(), |card| card.message.clone());
            let remediation = card.map(|card| card.remediation.clone());

            serde_json::json!({
                "id": id.to_string(),
                "name": id.to_string(),
                "shortDescription": { "text": message },
                // The remediation, because a code-scanning alert that says what is wrong and
                // not what to do about it is half an alert.
                "fullDescription": { "text": remediation.clone().unwrap_or_default() },
                "help": {
                    "text": remediation.unwrap_or_default(),
                },
                // No `properties.problem.severity`: severity is per violation here, not per
                // rule — the same rule can be an error in one config and a warning in
                // another — and `level` on each result is what a consumer reads anyway. A
                // rule-level default would have to be invented, and inventing one to fill a
                // field nothing requires is how a report starts lying.
            })
        })
        .collect();

    let results: Vec<serde_json::Value> = violations
        .iter()
        .map(|violation| {
            let index = fired
                .iter()
                .position(|id| *id == &violation.rule_id)
                .unwrap_or(0);

            serde_json::json!({
                "ruleId": violation.rule_id.to_string(),
                "ruleIndex": index,
                "level": sarif_level(violation.severity),
                "message": { "text": violation.message.clone() },
                "locations": [{
                    "physicalLocation": {
                        // Relative, with forward slashes, which `FilePath` already
                        // guarantees. An absolute path would point at a checkout directory
                        // that does not exist on the machine reading the report.
                        "artifactLocation": { "uri": violation.location.file.as_str() },
                        "region": {
                            "startLine": violation.location.position.line,
                            "startColumn": violation.location.position.column,
                        },
                    },
                }],
            })
        })
        .collect();

    let document = serde_json::json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "lanekeep",
                    "informationUri": "https://github.com/fmsouza/lanekeep",
                    "version": env!("CARGO_PKG_VERSION"),
                    "rules": rules,
                },
            },
            "results": results,
        }],
    });

    serde_json::to_string_pretty(&document).map_or_else(
        |_| String::from("{\"version\":\"2.1.0\",\"runs\":[]}\n"),
        |mut rendered| {
            rendered.push('\n');
            rendered
        },
    )
}

/// SARIF's level vocabulary.
///
/// `off` should never reach a reporter — a rule set to `off` is dropped at preparation — but
/// mapping it to `note` rather than panicking keeps a reporter from being the thing that
/// fails a run.
const fn sarif_level(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warning",
        Severity::Off => "note",
    }
}

/// Token-minimal, remediation-first, grouped by rule.
///
/// Written for something that pays per token and acts on what it reads, which makes it a
/// different document from the human one rather than a terser rendering of it:
///
/// - **Grouped by rule, not by file.** Twelve violations of one rule are one thing to learn
///   and one fix to apply. Grouped by file they read as twelve problems.
/// - **The card once per rule**, not once per violation. It is the largest part of the
///   output and repeating it is the single biggest waste available.
/// - **Remediation before locations.** What to do comes before where, because the locations
///   are only useful once the fix is known.
/// - **Only rules that fired.** A rule with no violations has nothing to teach right now.
fn agent(violations: &[Violation], cards: &Cards) -> String {
    if violations.is_empty() {
        return String::from("No violations.\n");
    }

    // `BTreeMap` for grouping, so rules come out in the same order as the canonical
    // violation sort and two runs produce identical bytes.
    let mut grouped: BTreeMap<&RuleId, Vec<&Violation>> = BTreeMap::new();
    for violation in violations {
        grouped
            .entry(&violation.rule_id)
            .or_default()
            .push(violation);
    }

    let mut out = format!(
        "{} violation(s) across {} rule(s).\n",
        violations.len(),
        grouped.len()
    );

    for (id, found) in &grouped {
        let first = found.first();
        let card = cards.get(*id);

        // The card's message heads the section when there is one. A rule that varies its
        // message per violation — "importing 'lodash' is restricted" — would otherwise have
        // one arbitrary violation's text standing for the whole group, and the rest listed
        // as exceptions to it.
        let message = card.map_or_else(
            || first.map_or_else(String::new, |v| v.message.clone()),
            |card| card.message.clone(),
        );
        let remediation = card.map_or_else(
            || first.map_or_else(String::new, |v| v.remediation.clone()),
            |card| card.remediation.clone(),
        );

        let _ = write!(out, "\n## {id}\n{message}\nFix: {remediation}\n");

        // Examples come from the card, which a violation does not carry. Absent when the
        // caller had none — a reporter that required them would be unusable from anywhere
        // that only has violations, which is every consumer of the JSON output.
        if let Some(card) = card {
            let _ = write!(
                out,
                "Bad:  {}\nGood: {}\n",
                card.examples.bad, card.examples.good
            );
        }

        out.push_str("At:\n");
        for violation in found {
            // One line each, no repeated message: the message is the rule's, stated above.
            // A per-violation message that differs from the rule's is worth keeping, though.
            let location = format!(
                "  {}:{}:{}",
                violation.location.file.as_str(),
                violation.location.position.line,
                violation.location.position.column
            );
            if violation.message == message {
                let _ = writeln!(out, "{location}");
            } else {
                let _ = writeln!(out, "{location} — {}", violation.message);
            }
        }
    }

    out
}

/// The exit code for a run, per §11.
#[must_use]
pub fn exit_code(violations: &[Violation], warn_only: bool) -> i32 {
    i32::from(!warn_only && violations.iter().any(|v| v.severity == Severity::Error))
}

#[cfg(test)]
mod tests {
    use lanekeep_core::{Examples, FilePath, Location, Position, RuleId};

    use super::*;

    fn violation(rule: &str, file: &str, line: u32, severity: Severity) -> Violation {
        Violation {
            rule_id: rule.parse::<RuleId>().expect("valid"),
            location: Location::new(FilePath::new(file), Position::new(line, 1)),
            message: "something is wrong".to_owned(),
            remediation: "do it the other way".to_owned(),
            severity,
            fix: None,
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
            &Cards::new(),
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
            &Cards::new(),
        );
        assert!(out.contains('\x1b'), "should contain escapes");
    }

    #[test]
    fn a_clean_run_says_so() {
        let out = render(Format::Human, Color::Never, &[], summary(), &Cards::new());
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
            &Cards::new(),
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
            &Cards::new(),
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
            &Cards::new(),
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
            &Cards::new(),
        );
        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn json_for_a_clean_run_is_still_a_full_document() {
        let out = render(Format::Json, Color::Never, &[], summary(), &Cards::new());
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
            let first = render(format, Color::Never, &violations, summary(), &Cards::new());
            for _ in 0..5 {
                assert_eq!(
                    render(format, Color::Never, &violations, summary(), &Cards::new()),
                    first
                );
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
        // The unrecognized value comes back so the caller can name it in a diagnostic.
        // `sarif` was the example here until sarif existed; the point is the mechanism.
        assert_eq!(Format::parse("checkstyle"), Err("checkstyle".to_owned()));
    }

    // --- sarif -------------------------------------------------------------------------

    fn cards_for(id: &str) -> Cards {
        let mut cards = Cards::new();
        cards.insert(
            id.parse::<RuleId>().expect("valid"),
            RuleCard {
                message: "something is wrong".to_owned(),
                remediation: "do it the other way".to_owned(),
                examples: Examples {
                    bad: "const x = bad();".to_owned(),
                    good: "const x = good();".to_owned(),
                },
            },
        );
        cards
    }

    fn sarif_of(violations: &[Violation], cards: &Cards) -> serde_json::Value {
        let out = render(Format::Sarif, Color::Never, violations, summary(), cards);
        serde_json::from_str(&out).expect("valid json")
    }

    #[test]
    fn sarif_declares_its_version_and_schema() {
        let doc = sarif_of(
            &[violation("local/a", "src/a.ts", 4, Severity::Error)],
            &Cards::new(),
        );
        assert_eq!(doc["version"], "2.1.0");
        assert!(
            doc["$schema"].as_str().is_some_and(|s| s.contains("sarif")),
            "{doc}"
        );
    }

    #[test]
    fn sarif_places_a_violation_at_its_line_and_column() {
        let doc = sarif_of(
            &[violation("local/a", "src/a.ts", 4, Severity::Error)],
            &Cards::new(),
        );
        let location = &doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"];
        assert_eq!(location["artifactLocation"]["uri"], "src/a.ts");
        assert_eq!(location["region"]["startLine"], 4);
        assert_eq!(location["region"]["startColumn"], 1);
    }

    #[test]
    fn sarif_maps_severity_to_its_own_vocabulary() {
        for (severity, expected) in [
            (Severity::Error, "error"),
            (Severity::Warn, "warning"),
            (Severity::Off, "note"),
        ] {
            let doc = sarif_of(
                &[violation("local/a", "src/a.ts", 1, severity)],
                &Cards::new(),
            );
            assert_eq!(doc["runs"][0]["results"][0]["level"], expected);
        }
    }

    #[test]
    fn sarif_describes_each_rule_once_and_indexes_into_it() {
        // The format's own shape, and what keeps a card from being repeated per violation.
        let violations = [
            violation("local/a", "src/a.ts", 1, Severity::Error),
            violation("local/a", "src/b.ts", 2, Severity::Error),
            violation("local/b", "src/c.ts", 3, Severity::Warn),
        ];
        let doc = sarif_of(&violations, &cards_for("local/a"));

        let rules = doc["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules array");
        assert_eq!(rules.len(), 2, "two rules fired, not three violations");

        let results = doc["runs"][0]["results"].as_array().expect("results");
        assert_eq!(results.len(), 3);
        for result in results {
            let index =
                usize::try_from(result["ruleIndex"].as_u64().expect("an index")).expect("fits");
            assert_eq!(
                rules[index]["id"], result["ruleId"],
                "ruleIndex points at a different rule than ruleId names"
            );
        }
    }

    #[test]
    fn sarif_lists_only_rules_that_fired() {
        // A configured rule with nothing to say would be noise in a diff view.
        let doc = sarif_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Error)],
            &cards_for("local/unrelated"),
        );
        let rules = doc["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules array");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["id"], "local/a");
    }

    #[test]
    fn sarif_carries_the_remediation_into_help() {
        // An alert that says what is wrong and not what to do about it is half an alert.
        let doc = sarif_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Error)],
            &cards_for("local/a"),
        );
        let rule = &doc["runs"][0]["tool"]["driver"]["rules"][0];
        assert_eq!(rule["help"]["text"], "do it the other way");
    }

    #[test]
    fn sarif_is_valid_with_no_violations() {
        let doc = sarif_of(&[], &Cards::new());
        assert_eq!(doc["runs"][0]["results"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn sarif_paths_stay_relative() {
        // An absolute path would name a checkout directory that does not exist on the
        // machine reading the report.
        let doc = sarif_of(
            &[violation("local/a", "src/deep/a.ts", 1, Severity::Error)],
            &Cards::new(),
        );
        let uri = doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]
            ["artifactLocation"]["uri"]
            .as_str()
            .expect("a uri");
        assert_eq!(uri, "src/deep/a.ts");
        assert!(!uri.starts_with('/'), "{uri}");
    }

    // --- agent -------------------------------------------------------------------------

    fn agent_of(violations: &[Violation], cards: &Cards) -> String {
        render(Format::Agent, Color::Never, violations, summary(), cards)
    }

    #[test]
    fn agent_groups_by_rule_rather_than_by_file() {
        // Twelve violations of one rule are one thing to learn and one fix to apply.
        let violations = [
            violation("local/a", "src/a.ts", 1, Severity::Error),
            violation("local/a", "src/b.ts", 2, Severity::Error),
            violation("local/b", "src/a.ts", 3, Severity::Warn),
        ];
        let out = agent_of(&violations, &Cards::new());

        assert_eq!(out.matches("## local/a").count(), 1, "{out}");
        assert_eq!(out.matches("## local/b").count(), 1, "{out}");

        let a = out.find("## local/a").expect("has local/a");
        let b = out.find("## local/b").expect("has local/b");
        assert!(a < b, "rules are not in canonical order:\n{out}");
    }

    #[test]
    fn agent_states_the_remediation_once_per_rule() {
        // The card is the largest part of the output; repeating it per violation is the
        // single biggest waste available.
        let violations = [
            violation("local/a", "src/a.ts", 1, Severity::Error),
            violation("local/a", "src/b.ts", 2, Severity::Error),
            violation("local/a", "src/c.ts", 3, Severity::Error),
        ];
        let out = agent_of(&violations, &Cards::new());
        assert_eq!(out.matches("do it the other way").count(), 1, "{out}");
    }

    #[test]
    fn agent_puts_the_fix_before_the_locations() {
        // What to do comes before where: the locations are only useful once the fix is
        // known.
        let out = agent_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Error)],
            &Cards::new(),
        );
        let fix = out.find("Fix:").expect("has a fix");
        let at = out.find("At:").expect("has locations");
        assert!(fix < at, "{out}");
    }

    #[test]
    fn agent_includes_examples_when_a_card_is_available() {
        let out = agent_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Error)],
            &cards_for("local/a"),
        );
        assert!(out.contains("const x = bad();"), "{out}");
        assert!(out.contains("const x = good();"), "{out}");
    }

    #[test]
    fn agent_works_without_cards() {
        // Every consumer that only has violations — the JSON output, a test — must still
        // get something useful.
        let out = agent_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Error)],
            &Cards::new(),
        );
        assert!(out.contains("## local/a"), "{out}");
        assert!(out.contains("Fix: do it the other way"), "{out}");
        assert!(!out.contains("Bad:"), "{out}");
    }

    #[test]
    fn agent_lists_every_location() {
        let violations = [
            violation("local/a", "src/a.ts", 1, Severity::Error),
            violation("local/a", "src/b.ts", 22, Severity::Error),
        ];
        let out = agent_of(&violations, &Cards::new());
        assert!(out.contains("src/a.ts:1:1"), "{out}");
        assert!(out.contains("src/b.ts:22:1"), "{out}");
    }

    #[test]
    fn agent_keeps_a_message_that_differs_from_the_rules() {
        // A per-violation message carries information the rule's does not — which module
        // was restricted, which export was unused.
        let mut specific = violation("local/a", "src/a.ts", 1, Severity::Error);
        specific.message = "importing 'lodash' is restricted".to_owned();
        let out = agent_of(
            &[
                violation("local/a", "src/b.ts", 2, Severity::Error),
                specific,
            ],
            &Cards::new(),
        );
        assert!(out.contains("importing 'lodash' is restricted"), "{out}");
    }

    #[test]
    fn agent_says_so_when_there_is_nothing_to_say() {
        assert_eq!(agent_of(&[], &Cards::new()), "No violations.\n");
    }

    #[test]
    fn agent_is_smaller_than_the_human_report_for_repeated_violations() {
        // The whole premise of the format. If it is not smaller it is not worth having.
        let violations: Vec<Violation> = (1..=30)
            .map(|n| violation("local/a", "src/a.ts", n, Severity::Error))
            .collect();

        let agent = agent_of(&violations, &Cards::new());
        let human = render(
            Format::Human,
            Color::Never,
            &violations,
            summary(),
            &Cards::new(),
        );
        assert!(
            agent.len() < human.len() / 2,
            "agent {} bytes vs human {} bytes",
            agent.len(),
            human.len()
        );
    }

    // --- every format ------------------------------------------------------------------

    #[test]
    fn every_format_parses_from_its_name() {
        for (name, expected) in [
            ("human", Format::Human),
            ("json", Format::Json),
            ("sarif", Format::Sarif),
            ("agent", Format::Agent),
        ] {
            assert_eq!(Format::parse(name), Ok(expected), "{name}");
        }
        assert!(Format::parse("yaml").is_err());
    }

    #[test]
    fn every_format_is_byte_identical_across_runs() {
        // An agent reads the output twice and must not see reordering as change.
        let violations = [
            violation("local/b", "src/z.ts", 9, Severity::Warn),
            violation("local/a", "src/a.ts", 1, Severity::Error),
            violation("local/a", "src/b.ts", 2, Severity::Error),
        ];
        let cards = cards_for("local/a");

        for format in [Format::Human, Format::Json, Format::Sarif, Format::Agent] {
            let first = render(format, Color::Never, &violations, summary(), &cards);
            for _ in 0..3 {
                assert_eq!(
                    render(format, Color::Never, &violations, summary(), &cards),
                    first,
                    "{format:?} is not stable"
                );
            }
        }
    }

    #[test]
    fn agent_heads_a_section_with_the_cards_message_not_a_violations() {
        // A rule that varies its message per violation would otherwise have one arbitrary
        // violation's text standing for the whole group, with the rest listed as
        // exceptions to it.
        let mut one = violation("local/a", "src/a.ts", 1, Severity::Error);
        one.message = "importing 'lodash' is restricted".to_owned();
        let mut other = violation("local/a", "src/b.ts", 2, Severity::Error);
        other.message = "importing 'moment' is restricted".to_owned();

        let out = agent_of(&[one, other], &cards_for("local/a"));
        let header = out
            .lines()
            .skip_while(|l| !l.starts_with("## local/a"))
            .nth(1)
            .expect("a message line");
        assert_eq!(header, "something is wrong", "{out}");

        // And both specific messages survive, since each says something the card does not.
        assert!(out.contains("importing 'lodash' is restricted"), "{out}");
        assert!(out.contains("importing 'moment' is restricted"), "{out}");
    }

    #[test]
    fn sarif_does_not_invent_a_rule_level_severity() {
        // Severity is per violation, not per rule — the same rule is an error in one config
        // and a warning in another. Inventing a rule-level default to fill a field nothing
        // requires is how a report starts lying.
        let doc = sarif_of(
            &[violation("local/a", "src/a.ts", 1, Severity::Warn)],
            &cards_for("local/a"),
        );
        let rule = &doc["runs"][0]["tool"]["driver"]["rules"][0];
        assert!(
            rule.get("properties").is_none(),
            "a rule-level severity was invented: {rule}"
        );
        assert_eq!(doc["runs"][0]["results"][0]["level"], "warning");
    }
}
