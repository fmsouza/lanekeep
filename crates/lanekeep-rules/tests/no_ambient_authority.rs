//! `local/no-ambient-authority`, run through the real engine.
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
}

/// The rule id every violation carries.
const RULE_ID: &str = "local/no-ambient-authority";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str =
    "lanekeep opens no sockets at all, and shells out only from `changed.rs`, for git";

/// One case, written once and run against every engine this rule has an arm for.
///
/// One today, the component. The shape is what let the same expectations hold the
/// TypeScript implementation that preceded it, and is the seam the next arm attaches at.
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

/// Every case, run against every arm below.
///
/// Migrated from `crates/lanekeep-cli/tests/selfcheck.rs`'s `authority` group. The harness case
/// there — the `authority()` helper that built a tester — was scaffolding and is not migrated;
/// the five assertions it served are.
const CASES: &[Case] = &[
    // Nothing but changed.rs spawns a process.
    Case {
        name: "a_subprocess_import_is_reported",
        source: "use std::process::Command;\n",
        options: None,
        expected: Expected::ReportsAt(&[(1, 1)]),
    },
    // §13's no network, ever, is enforced rather than aspirational.
    Case {
        name: "a_socket_is_reported",
        source: "use std::net::TcpStream;\n",
        options: None,
        expected: Expected::ReportsAt(&[(1, 1)]),
    },
    // --since shells out to git, and that is the one place.
    Case {
        name: "the_git_caller_passes",
        source: "use std::process::Command;\n",
        options: Some(r#"{"allow": ["subject/input.rs"]}"#),
        expected: Expected::Accepts,
    },
    // Only network and subprocess are the boundary.
    Case {
        name: "ordinary_std_passes",
        source: "use std::collections::BTreeMap;\nuse std::fs;\n",
        options: None,
        expected: Expected::Accepts,
    },
    // `std::process` alone would also match `std::process::ExitCode`, which is how `main()`
    // reports its own exit status and reaches nothing outside the process. `process::Command`
    // is the capability the rule is actually about, and it still catches the qualified path.
    // Both imports in one subject so a `FORBIDDEN` list containing bare `std::process` — which
    // would report both lines — is what this test would catch.
    Case {
        name: "exit_code_is_not_reported_but_command_still_is",
        source: "use std::process::Command;\nuse std::process::ExitCode;\n",
        options: None,
        expected: Expected::ReportsAt(&[(1, 1)]),
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
    }
}

/// Hold every violation to the three fields no [`Expected`] variant pins.
///
/// **Measured rather than supposed.** With the component's `metadata()` returning
/// `id: "local/no-ambient-authority"`, `severity: "warn"` and a remediation reading
/// `MUTANT REMEDIATION`, every case in this table passed on both arms — so a table asserting
/// only positions and messages says nothing about half of what a violation is, in a migration
/// whose whole claim is that the two implementations report identically.
///
/// Each of the three is load-bearing somewhere a position is not. The rule id is what a
/// suppression comment names and what the canonical `(ruleId, file, line, column)` sort orders
/// on, so a wrong one reorders unrelated output. Severity decides the exit code. The remediation
/// is what `lanekeep explain` and the agent reporter print, and is the half of a card that says
/// what to do rather than what is wrong.
///
/// Checked on every violation of every case rather than in a case of its own, so a rule migrated
/// after this one inherits it without anyone remembering to.
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
/// `cargo test`. Splitting the table into a `#[test]` per case would let nextest's
/// process-per-test do this, and would need a macro to generate them from the table — which is
/// a lot of machinery, and puts the count of cases somewhere other than the table. The scope
/// below is the same parallelism without either cost. Each case already has a directory of its
/// own, which is what makes it safe: a tester is never shared, so there is nothing between the
/// threads to synchronize.
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

/// The rule: a WebAssembly component built from `rust-rules/no-ambient-authority/`.
///
/// A **project rule** (`local/` id), so it is not embedded in the binary the way a built-in is.
/// The tester reads the committed artifact `just rust-rules` produced from disk, and a path
/// reference contributes every rule the artifact hosts — which is one, so the index is
/// discarded and the whole component is the rule.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON, and why the bare case is not
/// "no call" but a call with `null`.
fn component(case: &Case) -> RuleTester {
    const COMPONENT: &[u8] = include_bytes!("../components/no-ambient-authority.wasm");
    match case.options {
        None => RuleTester::for_component("no-ambient-authority", COMPONENT, "rs"),
        Some(options) => {
            RuleTester::for_component_configured("no-ambient-authority", COMPONENT, "rs", options)
        }
    }
    .expect("builds")
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}
