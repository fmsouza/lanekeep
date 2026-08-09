//! `lanekeep/no-glob-import`, run through the real engine.
//!
//! # Why the cases are a table
//!
//! Every case is a [`Case`] in [`CASES`], and a test function runs the whole table against a
//! tester it is handed. That indirection is what made a second implementation of this rule
//! testable against *the same* expectations rather than against a second file that looks
//! reasonable.
//!
//! Two independently written test files could each look sensible and still assert different
//! things, and the difference would be invisible: a case only one of them has is a case the
//! other is not held to. A table cannot express that.
//!
//! **There is one arm again, and the table is what earned the right to say so.** The rule was
//! a TypeScript module; it is a component now, and the two ran side by side against every case
//! in this table until the swap landed. The indirection stays because the next migration needs
//! it and because a case written against a tester rather than against an engine is the shape
//! that survives one.
//!
//! So a case belongs here rather than in a `#[test]` of its own, and one that could only be
//! written for a single arm is worth stopping over rather than working around.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::fmt::Write as _;

use lanekeep_core::{Severity, Violation};
use lanekeep_testkit::{RuleTester, TestError};

/// What a case asserts about *which* violations were reported, and where.
///
/// Deliberately not the whole of what a case asserts: [`assert_identity`] holds every violation
/// of every case to the three fields none of these variants mentions.
enum Expected {
    /// Nothing at all.
    Accepts,
    /// Exactly these one-based positions, in order.
    ReportsAt(&'static [(u32, u32)]),
    /// Exactly these messages, in order.
    ReportsMessages(&'static [&'static str]),
}

/// The rule id every violation carries.
const RULE_ID: &str = "lanekeep/no-glob-import";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str = "name what you import, so a reader can tell where each name comes from";

/// One case, written once and run against both engines.
struct Case {
    /// What it is called in a failure report. Named as the `#[test]` function it replaces was,
    /// so a failure here is greppable against the history that motivated the case.
    name: &'static str,
    /// The subject file, written as `subject/input.rs`.
    source: &'static str,
    /// The rule's options, or `None` for the rule used bare.
    ///
    /// **Written as JSON, which is also a JavaScript object literal.** That is what lets one
    /// string serve both arms: the TypeScript path splices it into a config as `rule(<options>)`
    /// and the component path embeds it as the `options` value of a `lanekeep.json` entry, which
    /// crosses to `configure` as data. A JavaScript-only spelling — unquoted keys, single-quoted
    /// strings — would work on one arm and not the other, and would mean two option tables.
    options: Option<&'static str>,
    /// What the rule must report.
    expected: Expected,
}

/// Every case, run against both engines.
///
/// Several exist because an earlier version got them wrong. Those say so, because a case whose
/// reason is not written down is a case somebody deletes while tidying.
const CASES: &[Case] = &[
    Case {
        name: "a_named_use_passes",
        source: "use std::collections::HashMap;\n",
        options: None,
        expected: Expected::Accepts,
    },
    Case {
        name: "a_use_list_passes",
        source: "use std::io::{Read, Write};\n",
        options: None,
        expected: Expected::Accepts,
    },
    Case {
        name: "a_glob_is_reported",
        source: "use crate::models::*;\n",
        options: None,
        expected: Expected::ReportsAt(&[(1, 1)]),
    },
    // The one shape a glob is the intended spelling of. A project using one should not have
    // to suppress this on every file.
    Case {
        name: "a_prelude_glob_passes_by_default",
        source: "use std::prelude::v1::*;\nuse crate::prelude::*;\n",
        options: None,
        expected: Expected::Accepts,
    },
    // The option the rule's own JSDoc documents — `noGlobImport({ allow: [...] })` — and which
    // reached the handler as `undefined` on every run, because the default export was a plain
    // object rather than a factory.
    Case {
        name: "an_allowed_pattern_passes",
        source: "use crate::internal::*;\n",
        options: Some(r#"{"allow": ["crate::internal::*"]}"#),
        expected: Expected::Accepts,
    },
    // `allow` is a replacement, not an addition — the default is a fallback for a rule that was
    // given nothing. Asserted because the opposite reading is just as plausible from the source,
    // and silently keeping the default would make the case above look wider than it is.
    Case {
        name: "a_configured_allow_list_replaces_the_prelude_default",
        source: "use crate::prelude::*;\n",
        options: Some(r#"{"allow": ["crate::internal::*"]}"#),
        expected: Expected::ReportsAt(&[(1, 1)]),
    },
    // `super` must not match `super::*`: the pattern is anchored at both ends, so a bare prefix
    // is not a match. Load-bearing history — the anchoring is what makes `allow` narrow enough
    // to be safe.
    Case {
        name: "a_prefix_alone_does_not_match_a_glob_path",
        source: "use super::*;\n",
        options: Some(r#"{"allow": ["super"]}"#),
        expected: Expected::ReportsAt(&[(1, 1)]),
    },
    // tree-sitter-rust's `use_wildcard` is `(path '::')? '*'`, so the captured text already
    // ends in `::*`. Appending another produced `use crate::models::*::*` in every message this
    // rule ever reported, for its whole life until the precursor to this migration fixed it.
    // Nothing had caught it: no test asserted a message, and the default `*prelude*` pattern
    // matches either spelling. This is the case a Rust port must not regress — the message
    // assertion is the only thing that would catch the doubling coming back in translation.
    Case {
        name: "the_message_names_the_glob_once",
        source: "use crate::models::*;\n",
        options: None,
        expected: Expected::ReportsMessages(&[
            "`use crate::models::*` hides where every name in this file comes from",
        ]),
    },
    Case {
        name: "every_glob_in_a_file_is_reported",
        source: "use a::*;\nuse b::*;\n",
        options: None,
        expected: Expected::ReportsAt(&[(1, 1), (2, 1)]),
    },
];

/// Assert one case against a tester built for it.
///
/// One `run` rather than one per assertion. The harness's `accepts`/`reports_at`/
/// `reports_messages` each run the engine themselves and hand back only a verdict, and a case
/// needs both the verdict and the violations behind it — so the comparison is done here, against
/// one set of results. For the component arm that also halves the work: a run there compiles the
/// rule twice, and two runs per case would be four.
fn assert_case(tester: &RuleTester, case: &Case) -> Result<(), TestError> {
    let violations = tester.run(case.source)?;
    assert_identity(&violations, case)?;

    match case.expected {
        Expected::Accepts if violations.is_empty() => Ok(()),
        Expected::Accepts => Err(mismatch(
            case,
            "expected no violations",
            &rendered(&violations),
        )),
        Expected::ReportsAt(expected) => {
            let actual: Vec<(u32, u32)> = violations
                .iter()
                .map(|v| (v.location.position.line, v.location.position.column))
                .collect();
            if actual == expected {
                return Ok(());
            }
            Err(mismatch(
                case,
                &format!("positions {expected:?}"),
                &format!("{actual:?}"),
            ))
        }
        Expected::ReportsMessages(expected) => {
            let actual: Vec<&str> = violations.iter().map(|v| v.message.as_str()).collect();
            if actual == expected {
                return Ok(());
            }
            Err(mismatch(
                case,
                &format!("messages {expected:?}"),
                &format!("{actual:?}"),
            ))
        }
    }
}

/// Hold every violation to the three fields no [`Expected`] variant pins.
///
/// **Measured rather than supposed**, following the precedent `no_unwrap.rs` set: a component
/// whose `metadata()` returns a wrong id, severity or remediation can still satisfy a table that
/// only asserts positions and messages. Checked on every violation of every case rather than in
/// a case of its own, so a rule migrated after this one inherits it without anyone remembering
/// to add it.
///
/// Each of the three is load-bearing somewhere a position is not. The rule id is what a
/// suppression comment names and what the canonical `(ruleId, file, line, column)` sort orders
/// on, so a wrong one reorders unrelated output. Severity decides the exit code. The remediation
/// is what `lanekeep explain` and the agent reporter print, and is the half of a card that says
/// what to do rather than what is wrong.
fn assert_identity(violations: &[Violation], case: &Case) -> Result<(), TestError> {
    for violation in violations {
        let id = violation.rule_id.to_string();
        if id != RULE_ID {
            return Err(mismatch(case, &format!("rule id `{RULE_ID}`"), &id));
        }
        if violation.severity != SEVERITY {
            return Err(mismatch(
                case,
                &format!("severity {SEVERITY:?}"),
                &format!("{:?}", violation.severity),
            ));
        }
        if violation.remediation != REMEDIATION {
            return Err(mismatch(
                case,
                &format!("remediation `{REMEDIATION}`"),
                &format!("`{}`", violation.remediation),
            ));
        }
    }
    Ok(())
}

/// A failure naming both sides and the source that produced them.
fn mismatch(case: &Case, expected: &str, actual: &str) -> TestError {
    let source = case.source.lines().fold(String::new(), |mut out, line| {
        let _ = writeln!(out, "  | {line}");
        out
    });
    TestError::Mismatch(format!(
        "  expected: {expected}\n  actual:   {actual}\n\nsource:\n{source}"
    ))
}

/// Violations as one line each, for a case that expected none.
fn rendered(violations: &[Violation]) -> String {
    violations.iter().fold(String::new(), |mut out, violation| {
        let _ = write!(
            out,
            "{}:{} {}; ",
            violation.location.position.line, violation.location.position.column, violation.message
        );
        out
    })
}

/// Run the whole table, reporting every failure rather than only the first.
///
/// One `#[test]` per engine rather than one per case, because the table is the unit: a case
/// that exists for one engine and not the other is the thing this file is arranged to make
/// unrepresentable. Collecting the failures is what keeps that from costing anything — a
/// migration that gets three cases wrong says so once, rather than three runs in a row.
///
/// One engine today. The parameter is not vestigial: it is the seam a second arm is added at,
/// and this file has had two arms before.
///
/// # A thread per case, which nextest would otherwise have given for free
///
/// Every case builds its own throwaway project and runs the real engine over it, and for a
/// component that means compiling the rule twice — once when the config asks it what it is,
/// once when the engine prepares — with a Cranelift that is itself an unoptimized build under
/// `cargo test`. `no_unwrap.rs` measured 29.6s for a sequential run of its (larger) table against
/// 0.4s for the TypeScript arm it then had, in a recipe that runs on every commit.
///
/// Splitting the table into a `#[test]` per case would let nextest's process-per-test do this,
/// and would need a macro to generate them from the table — which is a lot of machinery, and
/// puts the count of cases somewhere other than the table. The scope below is the same
/// parallelism without either cost. Each case already has a directory of its own, which is what
/// makes it safe: a tester is never shared, so there is nothing between the threads to
/// synchronize.
fn assert_every_case(build: impl Fn(&Case) -> RuleTester + Sync) {
    let build = &build;
    let results: Vec<Option<String>> = std::thread::scope(|scope| {
        let running: Vec<_> = CASES
            .iter()
            .map(|case| scope.spawn(move || assert_case(&build(case), case)))
            .collect();
        running
            .into_iter()
            .zip(CASES)
            .map(|(thread, case)| match thread.join() {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("\n--- {} ---\n{error}\n", case.name)),
                // A panicked case is a failure like any other, and swallowing it would turn the
                // one failure that carries no `TestError` into a silent pass.
                Err(_) => Some(format!("\n--- {} ---\npanicked\n", case.name)),
            })
            .collect()
    });

    let mut failures = String::new();
    for reported in results.iter().flatten() {
        let _ = write!(failures, "{reported}");
    }
    assert!(failures.is_empty(), "{failures}");
}

/// The rule: a WebAssembly component built from `rust-rules/no-glob-import/`.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON, and why the bare case is not
/// "no call" but a call with `null`.
///
/// Both shapes have to work and for a while only one of them did: `lanekeep init` writes a bare
/// `"lanekeep/no-glob-import"` into a Rust project's config, while the usage the rule documents
/// configures it. That is why the table carries cases of each kind rather than only the
/// configured ones.
fn component(case: &Case) -> RuleTester {
    let bytes = lanekeep_rules::component("no-glob-import").expect("the component ships");
    match case.options {
        None => RuleTester::for_component("no-glob-import", bytes, "rs"),
        Some(options) => {
            RuleTester::for_component_configured("no-glob-import", bytes, "rs", options)
        }
    }
    .expect("builds")
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}
