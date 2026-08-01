//! Suppression directives: `lanekeep-ignore-next-line` and `lanekeep-ignore-file`.
//!
//! ```text
//! // lanekeep-ignore-next-line local/no-numeric-sizes reason: legacy API requires exact 44
//! minWidth: 44,
//!
//! // lanekeep-ignore-file local/no-primitive-components reason: generated fixture
//! ```
//!
//! # A suppression that does not work must say so
//!
//! The failure mode this module is arranged against is a directive that looks like it
//! silences something and does not — a typo in the rule id, a missing `reason:`, a
//! `lanekeep-ignore-nextline`. The author moves on believing the violation is handled, and
//! nothing ever tells them otherwise.
//!
//! So a malformed directive is **reported**, not skipped. Every field that could be
//! mistyped is either required or checked, and the diagnostic names what was wrong.
//!
//! # Why `reason:` is mandatory
//!
//! A suppression is a decision to accept a violation, and the next person to read it needs
//! to know whether that decision still holds. Without a reason it is indistinguishable from
//! someone silencing a diagnostic to make a build pass.
//!
//! # Scanning text, not the tree
//!
//! Directives are found by scanning the source for a standalone token rather than by
//! walking comments in the parse tree. That is what §10 specifies, it costs one pass over
//! bytes already in memory, and it works for a file that failed to parse.
//!
//! The cost is that a directive inside a string literal counts. That is a strange thing to
//! write and the consequence is a suppression that does nothing visible, which the unused
//! report surfaces.

use crate::rule_id::RuleId;

/// The token introducing a directive that covers the following line.
const NEXT_LINE: &str = "lanekeep-ignore-next-line";

/// The token introducing a directive that covers the whole file.
const WHOLE_FILE: &str = "lanekeep-ignore-file";

/// What a directive covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The line after the directive.
    NextLine,
    /// Every line in the file.
    File,
}

/// A calendar date, for `expires:`.
///
/// Deliberately not a general date type: it exists to be parsed from `YYYY-MM-DD`, compared,
/// and printed. Comparison is on the tuple, which is why the fields are in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    /// Four-digit year.
    pub year: u16,
    /// One-based month.
    pub month: u8,
    /// One-based day.
    pub day: u8,
}

impl Date {
    /// Parse `YYYY-MM-DD`.
    ///
    /// Rejects anything else, including a real date written another way. A directive whose
    /// expiry could not be read would otherwise never expire, which is the one outcome an
    /// expiry exists to prevent.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }

        let year: u16 = text.get(0..4)?.parse().ok()?;
        let month: u8 = text.get(5..7)?.parse().ok()?;
        let day: u8 = text.get(8..10)?.parse().ok()?;

        // Range-checked but not calendar-checked: 2026-02-30 is accepted. Validating month
        // lengths would need leap-year rules for no benefit — the date is only ever
        // compared, and an impossible date compares perfectly sensibly.
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }

        Some(Self { year, month, day })
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A directive that parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    /// What it covers.
    pub scope: Scope,
    /// The rules it silences. Never empty — a directive naming none is malformed.
    pub rules: Vec<RuleId>,
    /// Why, as the author wrote it.
    pub reason: String,
    /// When it stops applying, if it says.
    pub expires: Option<Date>,
    /// One-based line the directive is on.
    pub line: u32,
    /// One-based column the directive starts at.
    pub column: u32,
}

impl Suppression {
    /// Whether this directive covers a violation of `rule` at `line`.
    #[must_use]
    pub fn covers(&self, rule: &RuleId, line: u32) -> bool {
        let in_scope = match self.scope {
            Scope::File => true,
            // The line after the directive. Not the directive's own line: a trailing
            // comment on the offending line would be a different form, and supporting both
            // silently would make it unclear which one a reader is looking at.
            Scope::NextLine => line == self.line + 1,
        };
        in_scope && self.rules.contains(rule)
    }
}

/// A directive that did not parse, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// What is wrong, phrased for the person who wrote it.
    pub problem: String,
}

/// Everything a file's directives amount to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suppressions {
    /// Directives that parsed.
    pub valid: Vec<Suppression>,
    /// Directives that did not.
    pub malformed: Vec<Malformed>,
}

impl Suppressions {
    /// Whether anything here could silence a violation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.valid.is_empty() && self.malformed.is_empty()
    }

    /// The index of the directive covering a violation, if one does.
    ///
    /// The index rather than a boolean, so a caller can record which directives were used
    /// and report the rest as unused.
    #[must_use]
    pub fn covering(&self, rule: &RuleId, line: u32) -> Option<usize> {
        self.valid
            .iter()
            .position(|suppression| suppression.covers(rule, line))
    }
}

/// Find every directive in a file.
///
/// Never fails: a directive that cannot be understood becomes a [`Malformed`] entry rather
/// than an error, because one bad comment must not stop a file from being checked.
#[must_use]
pub fn parse(source: &str) -> Suppressions {
    let mut found = Suppressions::default();

    for (index, text) in source.lines().enumerate() {
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);

        let Some((scope, at)) = find_directive(text) else {
            continue;
        };
        let column = u32::try_from(at + 1).unwrap_or(u32::MAX);

        let token = match scope {
            Scope::NextLine => NEXT_LINE,
            Scope::File => WHOLE_FILE,
        };
        let rest = text.get(at + token.len()..).unwrap_or_default();

        match parse_body(scope, rest, line, column) {
            Ok(suppression) => found.valid.push(suppression),
            Err(problem) => found.malformed.push(Malformed {
                line,
                column,
                problem,
            }),
        }
    }

    found
}

/// Locate a directive token in a line, if it stands alone.
///
/// "Stands alone" means not adjacent to a word character on either side, which is what stops
/// prose about `lanekeep-ignore-next-line-ish` from matching. `lanekeep-ignore-file` is
/// checked first because it is not a prefix of the other, so order is only a matter of
/// finding the earlier one.
fn find_directive(text: &str) -> Option<(Scope, usize)> {
    let next_line = standalone(text, NEXT_LINE).map(|at| (Scope::NextLine, at));
    let whole_file = standalone(text, WHOLE_FILE).map(|at| (Scope::File, at));

    match (next_line, whole_file) {
        (Some(a), Some(b)) => Some(if a.1 <= b.1 { a } else { b }),
        (found, None) | (None, found) => found,
    }
}

/// The offset of `token` in `text`, if it appears as a standalone word.
fn standalone(text: &str, token: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(offset) = text.get(from..)?.find(token) {
        let at = from + offset;
        let before = text[..at].chars().next_back();
        let after = text[at + token.len()..].chars().next();

        let bounded = !before.is_some_and(is_word)
            // A trailing `-` would make this `lanekeep-ignore-file-later`, which is not the
            // directive and must not be treated as one.
            && !after.is_some_and(|c| is_word(c) || c == '-');

        if bounded {
            return Some(at);
        }
        from = at + token.len();
    }
    None
}

const fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Parse everything after the directive token.
fn parse_body(scope: Scope, rest: &str, line: u32, column: u32) -> Result<Suppression, String> {
    // `reason:` splits the directive: rule ids before, prose after. Splitting on the keyword
    // rather than on whitespace is what lets a reason contain anything, including a colon.
    let Some((ids, tail)) = rest.split_once("reason:") else {
        return Err(format!(
            "suppression has no `reason:` — a suppression is a decision to accept a \
             violation, and the next person to read it cannot tell whether it still holds \
             without one\n  write: {} <rule-id> reason: why this is acceptable",
            token_for(scope)
        ));
    };

    let rules = parse_rules(ids)?;

    // `expires:` may follow the reason. Taken from the end so the reason keeps any text
    // before it — a reason is prose and must not be truncated by a word appearing in it.
    let (reason, expires) = match tail.rsplit_once("expires:") {
        Some((before, date)) => {
            let text = date.trim();
            let Some(parsed) = Date::parse(text) else {
                return Err(format!(
                    "suppression has an unreadable `expires: {text}` — expected \
                     YYYY-MM-DD\n  an expiry that cannot be read would never expire, which \
                     is the one thing an expiry exists to prevent"
                ));
            };
            (before.trim(), Some(parsed))
        }
        None => (tail.trim(), None),
    };

    if reason.is_empty() {
        return Err(format!(
            "suppression has an empty `reason:`\n  write: {} <rule-id> reason: why this is \
             acceptable",
            token_for(scope)
        ));
    }

    Ok(Suppression {
        scope,
        rules,
        reason: reason.to_owned(),
        expires,
        line,
        column,
    })
}

/// Parse the rule ids between the directive and `reason:`.
fn parse_rules(text: &str) -> Result<Vec<RuleId>, String> {
    let mut rules = Vec::new();

    for token in text.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        match token.parse::<RuleId>() {
            Ok(id) => rules.push(id),
            Err(_) => {
                return Err(format!(
                    "`{token}` is not a rule id\n  ids are namespaced — `lanekeep/<name>` \
                     for built-in rules, `local/<name>` for this project's"
                ));
            }
        }
    }

    if rules.is_empty() {
        return Err(String::from(
            "suppression names no rules\n  a directive that silenced everything would hide \
             violations nobody chose to accept — name the rules it is for",
        ));
    }

    Ok(rules)
}

const fn token_for(scope: Scope) -> &'static str {
    match scope {
        Scope::NextLine => NEXT_LINE,
        Scope::File => WHOLE_FILE,
    }
}

/// Today, from the host clock.
///
/// The one place lanekeep looks at the clock, and it is deliberately here rather than in the
/// sandbox: a rule must not be able to observe the date, but a suppression's expiry has to
/// be compared against something. Callers fix it once per run so two files checked a
/// millisecond apart cannot disagree about what day it is — and any file whose result
/// depends on it says so in its cache key.
///
/// UTC, not local time. A deadline that moved with the reader's time zone would expire
/// twice in some places and not at all in others.
#[must_use]
pub fn today() -> Date {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    from_unix_days(i64::try_from(seconds / 86_400).unwrap_or(0))
}

/// Civil date from a count of days since 1970-01-01.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the whole proleptic Gregorian
/// range and needs no table. Written out rather than pulled in: one function against a
/// dependency that would carry formatting, parsing and time zones for a date this only ever
/// compares.
fn from_unix_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };

    Date {
        year: u16::try_from(if month <= 2 { year + 1 } else { year }).unwrap_or(1970),
        month: u8::try_from(month).unwrap_or(1),
        day: u8::try_from(day).unwrap_or(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str) -> RuleId {
        id.parse().expect("valid id")
    }

    fn only(source: &str) -> Suppression {
        let found = parse(source);
        assert!(
            found.malformed.is_empty(),
            "unexpectedly malformed: {:?}",
            found.malformed
        );
        assert_eq!(found.valid.len(), 1, "{:?}", found.valid);
        found.valid.into_iter().next().expect("one")
    }

    fn problem(source: &str) -> String {
        let found = parse(source);
        assert!(
            found.valid.is_empty(),
            "unexpectedly valid: {:?}",
            found.valid
        );
        assert_eq!(found.malformed.len(), 1, "{:?}", found.malformed);
        found.malformed.into_iter().next().expect("one").problem
    }

    #[test]
    fn a_next_line_directive_parses() {
        let found = only("// lanekeep-ignore-next-line local/a reason: legacy\nminWidth: 44,\n");
        assert_eq!(found.scope, Scope::NextLine);
        assert_eq!(found.rules, vec![rule("local/a")]);
        assert_eq!(found.reason, "legacy");
        assert_eq!(found.line, 1);
        assert_eq!(found.expires, None);
    }

    #[test]
    fn a_file_directive_parses() {
        let found = only("// lanekeep-ignore-file local/a reason: generated fixture\n");
        assert_eq!(found.scope, Scope::File);
        assert_eq!(found.reason, "generated fixture");
    }

    #[test]
    fn several_rules_may_be_named() {
        let found = only("// lanekeep-ignore-next-line local/a, local/b lanekeep/c reason: x\n");
        assert_eq!(
            found.rules,
            vec![rule("local/a"), rule("local/b"), rule("lanekeep/c")]
        );
    }

    #[test]
    fn an_expiry_parses_and_leaves_the_reason_intact() {
        let found = only(
            "// lanekeep-ignore-file local/a reason: waiting on the rewrite expires: 2026-12-31\n",
        );
        assert_eq!(found.reason, "waiting on the rewrite");
        assert_eq!(
            found.expires,
            Some(Date {
                year: 2026,
                month: 12,
                day: 31
            })
        );
    }

    #[test]
    fn a_reason_may_contain_a_colon() {
        // Splitting on the `reason:` keyword rather than on whitespace is what allows this.
        let found = only("// lanekeep-ignore-file local/a reason: see ticket ABC-1: the API\n");
        assert_eq!(found.reason, "see ticket ABC-1: the API");
    }

    #[test]
    fn a_missing_reason_is_malformed() {
        // The failure this module exists for: a directive that looks like it works.
        let text = problem("// lanekeep-ignore-next-line local/a\n");
        assert!(text.contains("no `reason:`"), "{text}");
    }

    #[test]
    fn an_empty_reason_is_malformed() {
        let text = problem("// lanekeep-ignore-next-line local/a reason:   \n");
        assert!(text.contains("empty"), "{text}");
    }

    #[test]
    fn naming_no_rules_is_malformed() {
        // A blanket suppression would hide violations nobody chose to accept.
        let text = problem("// lanekeep-ignore-next-line reason: everything\n");
        assert!(text.contains("names no rules"), "{text}");
    }

    #[test]
    fn a_bare_rule_id_is_malformed() {
        // Namespacing is a one-way door, and a bare id here would silently silence nothing.
        let text = problem("// lanekeep-ignore-next-line no-default-export reason: x\n");
        assert!(text.contains("not a rule id"), "{text}");
        assert!(text.contains("namespaced"), "{text}");
    }

    #[test]
    fn an_unreadable_expiry_is_malformed() {
        // An expiry that cannot be read would never expire, which is the one thing an
        // expiry exists to prevent.
        for bad in [
            "31-12-2026",
            "2026/12/31",
            "soon",
            "2026-13-01",
            "2026-12-32",
        ] {
            let text = problem(&format!(
                "// lanekeep-ignore-file local/a reason: x expires: {bad}\n"
            ));
            assert!(text.contains("unreadable"), "`{bad}` gave: {text}");
        }
    }

    #[test]
    fn prose_mentioning_the_directive_does_not_match() {
        // §10: the directive must be a standalone token.
        for prose in [
            "// use lanekeep-ignore-next-liner for this\n",
            "// see lanekeep-ignore-file-format docs\n",
            "// xlanekeep-ignore-file local/a reason: x\n",
        ] {
            let found = parse(prose);
            assert!(
                found.is_empty(),
                "prose matched as a directive: {prose:?} -> {found:?}"
            );
        }
    }

    #[test]
    fn a_directive_is_found_wherever_it_sits_on_the_line() {
        let found = only("const a = 1; // lanekeep-ignore-next-line local/a reason: x\n");
        assert_eq!(found.line, 1);
        assert!(found.column > 1, "column should point at the directive");
    }

    #[test]
    fn several_directives_in_one_file_all_parse() {
        let found = parse(
            "// lanekeep-ignore-file local/a reason: one\n\
             const x = 1;\n\
             // lanekeep-ignore-next-line local/b reason: two\n\
             const y = 2;\n",
        );
        assert_eq!(found.valid.len(), 2);
        assert_eq!(found.valid[0].line, 1);
        assert_eq!(found.valid[1].line, 3);
    }

    #[test]
    fn a_malformed_directive_does_not_stop_the_others() {
        // One bad comment must not stop a file from being checked.
        let found = parse(
            "// lanekeep-ignore-next-line local/a\n\
             const x = 1;\n\
             // lanekeep-ignore-next-line local/b reason: fine\n",
        );
        assert_eq!(found.valid.len(), 1);
        assert_eq!(found.malformed.len(), 1);
    }

    // --- what a directive covers ---------------------------------------------------------

    #[test]
    fn next_line_covers_the_following_line_only() {
        let found = only("// lanekeep-ignore-next-line local/a reason: x\nconst y = 1;\n");
        assert!(found.covers(&rule("local/a"), 2));
        assert!(!found.covers(&rule("local/a"), 1), "not its own line");
        assert!(
            !found.covers(&rule("local/a"), 3),
            "not the line after that"
        );
    }

    #[test]
    fn a_directive_covers_only_the_rules_it_names() {
        let found = only("// lanekeep-ignore-next-line local/a reason: x\n");
        assert!(found.covers(&rule("local/a"), 2));
        assert!(!found.covers(&rule("local/b"), 2));
    }

    #[test]
    fn file_scope_covers_every_line() {
        let found = only("// lanekeep-ignore-file local/a reason: x\n");
        for line in [1, 2, 500] {
            assert!(found.covers(&rule("local/a"), line));
        }
    }

    #[test]
    fn covering_reports_which_directive_matched() {
        // The index, not a boolean, so a caller can tell which directives went unused.
        let found = parse(
            "// lanekeep-ignore-next-line local/a reason: one\n\
             const x = 1;\n\
             // lanekeep-ignore-next-line local/b reason: two\n\
             const y = 2;\n",
        );
        assert_eq!(found.covering(&rule("local/a"), 2), Some(0));
        assert_eq!(found.covering(&rule("local/b"), 4), Some(1));
        assert_eq!(found.covering(&rule("local/c"), 2), None);
    }

    // --- dates ----------------------------------------------------------------------------

    #[test]
    fn dates_compare_chronologically() {
        let earlier = Date::parse("2026-01-31").expect("valid");
        let later = Date::parse("2026-02-01").expect("valid");
        assert!(earlier < later);

        let next_year = Date::parse("2027-01-01").expect("valid");
        assert!(later < next_year);
    }

    #[test]
    fn known_epochs_convert_correctly() {
        // Fixed points, including a leap day and a century boundary, so the conversion is
        // checked against something other than itself.
        for (days, expected) in [
            (0, "1970-01-01"),
            (18_993, "2022-01-01"),
            (19_051, "2022-02-28"),
            (11_016, "2000-02-29"),
            (20_666, "2026-08-01"),
        ] {
            assert_eq!(from_unix_days(days).to_string(), expected, "day {days}");
        }
    }

    #[test]
    fn today_is_a_plausible_date() {
        let now = today();
        assert!(now.year >= 2024 && now.year < 2200, "{now}");
        assert!((1..=12).contains(&now.month), "{now}");
        assert!((1..=31).contains(&now.day), "{now}");
    }

    #[test]
    fn a_date_renders_back_to_its_input() {
        assert_eq!(
            Date::parse("2026-08-01").expect("valid").to_string(),
            "2026-08-01"
        );
    }
}
