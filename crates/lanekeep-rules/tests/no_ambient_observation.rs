//! `local/no-ambient-observation`, run through the real engine.
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
//! written for a single arm is worth stopping over rather than working around. The one case
//! that does not fit the table — `an_empty_observation_scope_is_refused`, which asserts a
//! *load* failure rather than a set of violations — is a `#[test]` of its own at the bottom,
//! modeled on the selfcheck.rs original it was migrated from.

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
const RULE_ID: &str = "local/no-ambient-observation";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str = "take the value as a parameter, fixed once per run by the caller, so the cache key can account for it";

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
/// Migrated from `crates/lanekeep-cli/tests/selfcheck.rs`'s `observation` group. The harness
/// case there — the `observation()` helper that built a tester — was scaffolding and is not
/// migrated; the three assertions it served are. The fourth, `an_empty_observation_scope_is_refused`,
/// asserts a load failure rather than a set of violations and lives as a `#[test]` of its own
/// below.
const CASES: &[Case] = &[
    // A cached result computed from the clock is not reproducible.
    Case {
        name: "a_clock_read_is_reported",
        source: "fn go() {\n    let n = std::time::SystemTime::now();\n}\n",
        options: Some(r#"{"scope": ["subject/"], "allow": []}"#),
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    // The environment is not in the cache key.
    Case {
        name: "an_environment_read_is_reported",
        source: "fn go() {\n    let v = std::env::var(\"HOME\");\n}\n",
        options: Some(r#"{"scope": ["subject/"], "allow": []}"#),
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    // `suppression::today` is the one place lanekeep looks at the clock.
    Case {
        name: "the_one_clock_site_passes",
        source: "fn go() {\n    let n = std::time::SystemTime::now();\n}\n",
        options: Some(r#"{"scope": ["subject/"], "allow": ["subject/input.rs"]}"#),
        expected: Expected::Accepts,
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
/// `id: "local/no-ambient-observation"`, `severity: "warn"` and a remediation reading
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
fn assert_every_case(build: impl Fn(&Case) -> Option<RuleTester> + Sync) {
    let build = &build;
    let results: Vec<Option<String>> = std::thread::scope(|scope| {
        let running: Vec<_> = CASES
            .iter()
            .map(|case| {
                scope.spawn(move || {
                    let Some(tester) = build(case) else {
                        return Ok(());
                    };
                    assert_case(&tester, case)
                })
            })
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

/// The artifact `just rust-rules` built into `components/`, or `None` when absent.
/// When absent the test returns early; CI builds the artifact before running the gate.
fn component_bytes(name: &str) -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("components")
        .join(format!("{name}.wasm"));
    std::fs::read(path).ok()
}

/// The rule: a WebAssembly component built from `rust-rules/no-ambient-observation/`.
///
/// A **project rule** (`local/` id), so it is not embedded in the binary the way a built-in is.
/// The artifact is a build output that `just rust-rules` writes into `components/`, and the test
/// reads it from disk there — it is not embedded, and is not committed to git. When it is absent
/// (a checkout that has not run `just rust-rules`), the tests skip rather than fail.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON, and why the bare case is not
/// "no call" but a call with `null`.
fn component(case: &Case) -> Option<RuleTester> {
    let component = component_bytes("no-ambient-observation")?;
    Some(
        match case.options {
            None => RuleTester::for_component("no-ambient-observation", &component, "rs"),
            Some(options) => RuleTester::for_component_configured(
                "no-ambient-observation",
                &component,
                "rs",
                options,
            ),
        }
        .expect("builds"),
    )
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}

/// A rule scoped to nothing must refuse to load rather than check nothing.
///
/// `RuleTester::for_component_configured` only writes the fixture to disk; the factory is not
/// called until `run` loads the config, which is what `accepts` triggers below. The error
/// surfaces there, as a `TestError::Load`, not from `for_component_configured` itself.
#[test]
fn an_empty_observation_scope_is_refused() {
    let Some(component) = component_bytes("no-ambient-observation") else {
        return;
    };
    let error = RuleTester::for_component_configured(
        "no-ambient-observation",
        &component,
        "rs",
        r#"{"scope": [], "allow": []}"#,
    )
    .expect("the rule builds")
    .accepts("fn go() {}\n")
    .expect_err("a rule scoped to nothing must refuse to load rather than check nothing");
    // Assert on wording unique to the thrown message, not on a short word. `RuleTester`'s
    // temp directory embeds the tester's name and every `ConfigError`'s `Display` interpolates
    // that path, so `contains("scope")` would be satisfied by the path alone for any load
    // error at all — including one that has nothing to do with this guard.
    assert!(
        format!("{error}").contains("silently checks nothing"),
        "{error}"
    );
}
