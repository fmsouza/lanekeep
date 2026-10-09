//! Baselines: the violations a project has accepted, so a run fails only on new ones.
//!
//! Adopting a rule over an existing codebase otherwise means fixing its whole backlog in the
//! same change, or suppressing every site. A baseline records the backlog once; a later run
//! hides what it records and reports the rest (architecture §10.2).
//!
//! # Identity
//!
//! A violation carries a file and a position, nothing else — no span, no node — and the line
//! moves whenever anything above it changes. So an entry is keyed by the rule, the file and a
//! [`fingerprint`] of the reported line's *text*, normalized so that indentation and line
//! endings do not count. Text is what every violation has, including cached ones and the
//! reduce phase's, which have no tree to ask.
//!
//! The message is deliberately not part of the key: a lanekeep upgrade that rewords a message
//! must not turn a whole baseline into new violations.
//!
//! # Counting
//!
//! Entries are a multiset. Two violations on identical lines share a key with a count of two;
//! a third copy is new. Matching walks violations in the order given — canonical order, in
//! practice — so when a copy is added, the *later* one is reported. Text cannot say which copy
//! is the old one, and a deterministic answer is the one worth having.
//!
//! # What this module does not do
//!
//! Any IO. Source lines arrive through a closure, so the caller decides where bytes come from
//! and this stays testable over an in-memory tree. Nothing here touches the engine or the
//! cache: a baseline filters an outcome after the run, so a cached result is the same with or
//! without one.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::location::FilePath;
use crate::rule_id::{RuleId, SUPPRESSION_RULE};
use crate::violation::Violation;

/// The only format version this build reads or writes.
pub const VERSION: u32 = 1;

/// Hex characters of the hash kept per entry: 64 bits.
///
/// A collision only matters between two lines of one rule in one file, so this is generous;
/// the full digest would only make the committed file harder to read.
const FINGERPRINT_LEN: usize = 16;

/// Trim a line and collapse every run of whitespace inside it to one space.
///
/// `\r` is whitespace here, so a checkout with `core.autocrlf` fingerprints as one without.
#[must_use]
pub fn normalize(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The position-independent identity of a violation within its rule and file.
///
/// A domain-separated `blake3` of the rule, the file and the [`normalize`]d line, with a NUL
/// between each so that two splits of one concatenation cannot collide.
#[must_use]
pub fn fingerprint(rule: &RuleId, file: &FilePath, line: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-baseline-v1\0");
    hasher.update(rule.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(file.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(normalize(line).as_bytes());
    let hex = hasher.finalize().to_hex();
    hex.as_str()
        .get(..FINGERPRINT_LEN)
        .unwrap_or_default()
        .to_owned()
}

/// Whether a violation of this rule may be recorded in a baseline at all.
///
/// Everything but `lanekeep/suppression`, which architecture §10 says cannot be suppressed: a
/// baseline that swallowed an expired directive's report would make `expires:` meaningless.
/// `lanekeep/parse` can be suppressed, so it can be baselined.
#[must_use]
pub fn is_baselineable(rule: &RuleId) -> bool {
    rule.to_string() != SUPPRESSION_RULE
}

/// One accepted finding, `count` times over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The rule that reported it.
    pub rule: RuleId,
    /// The file it is in.
    pub file: FilePath,
    /// [`fingerprint`] of the reported line.
    pub fingerprint: String,
    /// How many violations share this key.
    pub count: u32,
}

/// A recorded set of accepted violations.
///
/// Unknown fields are refused rather than ignored: a misspelled `entries` would otherwise read
/// as an empty baseline and report the whole backlog, with nothing naming the typo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Format version; always [`VERSION`] when written by this build.
    pub version: u32,
    /// Entries, sorted by `(rule, file, fingerprint)` when written by this build.
    pub entries: Vec<Entry>,
}

/// What a run could have seen, so an entry it could not have seen is never called stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// The files the run checked, or `None` when it checked everything discovery selects.
    pub files: Option<BTreeSet<FilePath>>,
    /// Rules that did not run, though configured — cross-file rules under a narrowed selection.
    pub skipped_rules: BTreeSet<RuleId>,
}

impl Scope {
    fn covers(&self, entry: &Entry) -> bool {
        !self.skipped_rules.contains(&entry.rule)
            && self
                .files
                .as_ref()
                .is_none_or(|files| files.contains(&entry.file))
    }
}

/// A run's violations, split against a baseline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Violations the baseline does not cover, in the order they were given.
    pub new: Vec<Violation>,
    /// How many violations the baseline covered.
    pub baselined: usize,
    /// Entries that did not occur, each with the count that did not, sorted like a baseline.
    pub stale: Vec<Entry>,
}

type Key = (RuleId, FilePath, String);

impl Baseline {
    /// Record every baselineable violation.
    ///
    /// `line` answers the text of a one-based line in a file, or an empty string when there is
    /// none — a file deleted since, or a position past its end.
    #[must_use]
    pub fn record(
        violations: &[Violation],
        mut line: impl FnMut(&FilePath, u32) -> String,
    ) -> Self {
        let mut counts: BTreeMap<Key, u32> = BTreeMap::new();
        for violation in violations {
            if !is_baselineable(&violation.rule_id) {
                continue;
            }
            let key = key(violation, &mut line);
            let count = counts.entry(key).or_insert(0);
            *count = count.saturating_add(1);
        }
        Self {
            version: VERSION,
            entries: entries(counts),
        }
    }

    /// Split `violations` into the ones this baseline covers and the ones it does not.
    ///
    /// Duplicate entries — possible only in a hand-edited file — sum their counts. An entry
    /// with count left over is stale only when `scope` says the run could have seen it.
    #[must_use]
    pub fn apply(
        &self,
        violations: Vec<Violation>,
        mut line: impl FnMut(&FilePath, u32) -> String,
        scope: &Scope,
    ) -> Applied {
        let mut remaining: BTreeMap<Key, u32> = BTreeMap::new();
        for entry in &self.entries {
            if !is_baselineable(&entry.rule) {
                continue;
            }
            let key = (
                entry.rule.clone(),
                entry.file.clone(),
                entry.fingerprint.clone(),
            );
            let count = remaining.entry(key).or_insert(0);
            *count = count.saturating_add(entry.count);
        }

        let mut applied = Applied::default();
        for violation in violations {
            let covered = is_baselineable(&violation.rule_id)
                && match remaining.get_mut(&key(&violation, &mut line)) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        true
                    }
                    _ => false,
                };
            if covered {
                applied.baselined += 1;
            } else {
                applied.new.push(violation);
            }
        }

        applied.stale = entries(remaining)
            .into_iter()
            .filter(|entry| scope.covers(entry))
            .collect();
        applied
    }
}

fn key(violation: &Violation, line: &mut impl FnMut(&FilePath, u32) -> String) -> Key {
    let file = &violation.location.file;
    let text = line(file, violation.location.position.line);
    (
        violation.rule_id.clone(),
        file.clone(),
        fingerprint(&violation.rule_id, file, &text),
    )
}

/// Sorted, non-empty entries from counts — a `BTreeMap` is already in `(rule, file,
/// fingerprint)` order, which is what makes a rewritten baseline byte-identical.
fn entries(counts: BTreeMap<Key, u32>) -> Vec<Entry> {
    counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|((rule, file, fingerprint), count)| Entry {
            rule,
            file,
            fingerprint,
            count,
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::location::{Location, Position};
    use crate::severity::Severity;

    fn violation(rule: &str, file: &str, line: u32) -> Violation {
        Violation {
            rule_id: rule.parse().expect("valid rule id"),
            location: Location::new(FilePath::new(file), Position::new(line, 1)),
            message: "message".to_owned(),
            remediation: "remediation".to_owned(),
            severity: Severity::Error,
            fix: None,
        }
    }

    /// A tiny in-memory tree: file → its lines, one-based through the closure.
    struct Tree(BTreeMap<&'static str, Vec<&'static str>>);

    impl Tree {
        fn new(files: &[(&'static str, &[&'static str])]) -> Self {
            Self(files.iter().map(|(f, l)| (*f, l.to_vec())).collect())
        }

        fn line(&self, file: &FilePath, n: u32) -> String {
            self.0
                .get(file.as_str())
                .and_then(|lines| lines.get((n as usize).wrapping_sub(1)))
                .map(|s| (*s).to_owned())
                .unwrap_or_default()
        }
    }

    fn id(rule: &str) -> RuleId {
        rule.parse().expect("valid rule id")
    }

    fn lines(applied: &Applied) -> Vec<u32> {
        applied
            .new
            .iter()
            .map(|v| v.location.position.line)
            .collect()
    }

    #[test]
    fn normalize_ignores_indentation_and_inner_whitespace() {
        assert_eq!(normalize("    export   default\t1;  "), "export default 1;");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   \t "), "");
    }

    #[test]
    fn crlf_and_lf_fingerprint_alike() {
        // A checkout with `core.autocrlf` must not turn a whole baseline into new violations.
        let rule = id("local/a");
        let file = FilePath::new("src/a.ts");
        assert_eq!(
            fingerprint(&rule, &file, "export default 1;\r"),
            fingerprint(&rule, &file, "export default 1;")
        );
    }

    #[test]
    fn fingerprint_depends_on_rule_file_and_text() {
        let a = fingerprint(&id("local/a"), &FilePath::new("src/a.ts"), "x");
        assert_eq!(a.len(), 16, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(
            a,
            fingerprint(&id("local/b"), &FilePath::new("src/a.ts"), "x")
        );
        assert_ne!(
            a,
            fingerprint(&id("local/a"), &FilePath::new("src/b.ts"), "x")
        );
        assert_ne!(
            a,
            fingerprint(&id("local/a"), &FilePath::new("src/a.ts"), "y")
        );
        // The separators are what keep two splits of one concatenation apart.
        assert_ne!(
            fingerprint(&id("local/a"), &FilePath::new("b"), "c"),
            fingerprint(&id("local/a"), &FilePath::new("bc"), "")
        );
    }

    #[test]
    fn record_then_apply_hides_everything() {
        let tree = Tree::new(&[("src/a.ts", &["export default 1;", "export default 2;"])]);
        let found = vec![
            violation("local/a", "src/a.ts", 1),
            violation("local/a", "src/a.ts", 2),
        ];
        let baseline = Baseline::record(&found, |f, n| tree.line(f, n));
        let applied = baseline.apply(found, |f, n| tree.line(f, n), &Scope::default());
        assert!(applied.new.is_empty(), "{applied:?}");
        assert_eq!(applied.baselined, 2);
        assert!(applied.stale.is_empty(), "{applied:?}");
    }

    #[test]
    fn a_moved_line_stays_baselined() {
        let before = Tree::new(&[("src/a.ts", &["export default 1;"])]);
        let baseline = Baseline::record(&[violation("local/a", "src/a.ts", 1)], |f, n| {
            before.line(f, n)
        });

        let after = Tree::new(&[("src/a.ts", &["// a", "", "      export default 1;"])]);
        let applied = baseline.apply(
            vec![violation("local/a", "src/a.ts", 3)],
            |f, n| after.line(f, n),
            &Scope::default(),
        );
        assert!(applied.new.is_empty(), "{applied:?}");
        assert_eq!(applied.baselined, 1);
    }

    #[test]
    fn a_second_copy_is_new_and_it_is_the_later_one() {
        let before = Tree::new(&[("src/a.ts", &["f();"])]);
        let baseline = Baseline::record(&[violation("local/a", "src/a.ts", 1)], |f, n| {
            before.line(f, n)
        });

        let after = Tree::new(&[("src/a.ts", &["f();", "f();"])]);
        let applied = baseline.apply(
            vec![
                violation("local/a", "src/a.ts", 1),
                violation("local/a", "src/a.ts", 2),
            ],
            |f, n| after.line(f, n),
            &Scope::default(),
        );
        assert_eq!(lines(&applied), [2]);
        assert_eq!(applied.baselined, 1);
    }

    #[test]
    fn a_vanished_violation_is_stale_with_its_remaining_count() {
        let before = Tree::new(&[("src/a.ts", &["f();", "f();", "g();"])]);
        let baseline = Baseline::record(
            &[
                violation("local/a", "src/a.ts", 1),
                violation("local/a", "src/a.ts", 2),
                violation("local/a", "src/a.ts", 3),
            ],
            |f, n| before.line(f, n),
        );

        let after = Tree::new(&[("src/a.ts", &["f();"])]);
        let applied = baseline.apply(
            vec![violation("local/a", "src/a.ts", 1)],
            |f, n| after.line(f, n),
            &Scope::default(),
        );
        assert!(applied.new.is_empty(), "{applied:?}");
        let stale: Vec<(String, u32)> = applied
            .stale
            .iter()
            .map(|e| (e.file.to_string(), e.count))
            .collect();
        assert_eq!(stale.len(), 2, "{applied:?}");
        assert_eq!(stale.iter().map(|(_, c)| c).sum::<u32>(), 2, "{applied:?}");
    }

    #[test]
    fn scope_limits_stale_to_checked_files_and_rules_that_ran() {
        let tree = Tree::new(&[("src/a.ts", &["f();"]), ("src/b.ts", &["f();"])]);
        let baseline = Baseline::record(
            &[
                violation("local/a", "src/a.ts", 1),
                violation("local/a", "src/b.ts", 1),
                violation("local/cross", "src/a.ts", 1),
            ],
            |f, n| tree.line(f, n),
        );

        // Nothing occurred. Only `src/a.ts` was checked, and `local/cross` did not run.
        let scope = Scope {
            files: Some(BTreeSet::from([FilePath::new("src/a.ts")])),
            skipped_rules: BTreeSet::from([id("local/cross")]),
        };
        let applied = baseline.apply(Vec::new(), |f, n| tree.line(f, n), &scope);
        let stale: Vec<String> = applied
            .stale
            .iter()
            .map(|e| format!("{} {}", e.rule, e.file))
            .collect();
        assert_eq!(stale, ["local/a src/a.ts"]);

        // A full run sees everything, a deleted file's entries included.
        let applied = baseline.apply(Vec::new(), |f, n| tree.line(f, n), &Scope::default());
        assert_eq!(applied.stale.len(), 3, "{applied:?}");
    }

    #[test]
    fn suppression_violations_are_never_baselined() {
        // Architecture §10: `lanekeep/suppression` cannot be suppressed, or an expired
        // directive would stay silent forever behind a baseline.
        assert!(!is_baselineable(&id("lanekeep/suppression")));
        assert!(is_baselineable(&id("lanekeep/parse")));

        let tree = Tree::new(&[("src/a.ts", &["// a directive with no reason"])]);
        let found = vec![violation("lanekeep/suppression", "src/a.ts", 1)];
        let recorded = Baseline::record(&found, |f, n| tree.line(f, n));
        assert!(recorded.entries.is_empty(), "{recorded:?}");

        // And a hand-written entry for one does not filter it.
        let forged = Baseline {
            version: VERSION,
            entries: vec![Entry {
                rule: id("lanekeep/suppression"),
                file: FilePath::new("src/a.ts"),
                fingerprint: fingerprint(
                    &id("lanekeep/suppression"),
                    &FilePath::new("src/a.ts"),
                    "// a directive with no reason",
                ),
                count: 1,
            }],
        };
        let applied = forged.apply(found, |f, n| tree.line(f, n), &Scope::default());
        assert_eq!(applied.new.len(), 1, "{applied:?}");
        assert!(applied.stale.is_empty(), "{applied:?}");
    }

    #[test]
    fn duplicate_entries_sum() {
        let tree = Tree::new(&[("src/a.ts", &["f();", "f();"])]);
        let one = Baseline::record(&[violation("local/a", "src/a.ts", 1)], |f, n| {
            tree.line(f, n)
        });
        let mut doubled = one.clone();
        doubled.entries.extend(one.entries);

        let applied = doubled.apply(
            vec![
                violation("local/a", "src/a.ts", 1),
                violation("local/a", "src/a.ts", 2),
            ],
            |f, n| tree.line(f, n),
            &Scope::default(),
        );
        assert!(applied.new.is_empty(), "{applied:?}");
    }

    #[test]
    fn record_is_sorted_and_merged() {
        let tree = Tree::new(&[("src/a.ts", &["f();", "f();"]), ("src/b.ts", &["g();"])]);
        let recorded = Baseline::record(
            &[
                violation("local/z", "src/a.ts", 1),
                violation("local/a", "src/b.ts", 1),
                violation("local/a", "src/a.ts", 2),
                violation("local/a", "src/a.ts", 1),
            ],
            |f, n| tree.line(f, n),
        );
        let keys: Vec<String> = recorded
            .entries
            .iter()
            .map(|e| format!("{} {} {}", e.rule, e.file, e.count))
            .collect();
        assert_eq!(
            keys,
            [
                "local/a src/a.ts 2",
                "local/a src/b.ts 1",
                "local/z src/a.ts 1"
            ]
        );
        assert_eq!(recorded.version, VERSION);
    }

    #[test]
    fn missing_line_is_empty() {
        // A violation past the end of a file — or in a file deleted after the run — still
        // gets a fingerprint, and the same one twice.
        let tree = Tree::new(&[]);
        let found = vec![violation("local/a", "gone.ts", 7)];
        let baseline = Baseline::record(&found, |f, n| tree.line(f, n));
        assert_eq!(baseline.entries.len(), 1);
        let applied = baseline.apply(found, |f, n| tree.line(f, n), &Scope::default());
        assert!(applied.new.is_empty(), "{applied:?}");
    }

    #[test]
    fn round_trips_through_json() {
        let tree = Tree::new(&[("src/a.ts", &["f();"])]);
        let baseline = Baseline::record(&[violation("local/a", "src/a.ts", 1)], |f, n| {
            tree.line(f, n)
        });
        let json = serde_json::to_string(&baseline).expect("serializes");
        assert!(json.contains(r#""rule":"local/a""#), "{json}");
        assert!(json.contains(r#""file":"src/a.ts""#), "{json}");
        let back: Baseline = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, baseline);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let typo = r#"{"version": 1, "entry": []}"#;
        assert!(serde_json::from_str::<Baseline>(typo).is_err());
        let entry = r#"{"version": 1, "entries": [
            {"rule": "local/a", "file": "a.ts", "fingerprint": "00", "count": 1, "line": 3}
        ]}"#;
        assert!(serde_json::from_str::<Baseline>(entry).is_err());
    }
}
