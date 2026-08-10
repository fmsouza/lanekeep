//! `lanekeep/no-unwrap`, run through the real engine.
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
const RULE_ID: &str = "lanekeep/no-unwrap";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str =
    "propagate with `?` and a typed error, so the caller decides what a failure means";

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
/// Several exist because an earlier version got them wrong. Those say so, because a case whose
/// reason is not written down is a case somebody deletes while tidying.
const CASES: &[Case] = &[
    Case {
        name: "the_question_mark_passes",
        source: "fn f() -> Result<u8, E> {\n    let c = load()?;\n    Ok(c)\n}\n",
        options: None,
        expected: Expected::Accepts,
    },
    Case {
        name: "unwrap_is_reported",
        source: "fn f() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    Case {
        name: "expect_is_reported_too",
        source: "fn f() {\n    let c = load().expect(\"boom\");\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    // In a test, panicking *is* the failure mechanism. Reporting here would mean either a rule
    // nobody can turn on or a suppression on every assertion.
    Case {
        name: "a_test_function_passes",
        source: "#[test]\nfn works() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::Accepts,
    },
    // The exemption is `/\btest\b/` over the attribute's text, and these are its edges: one
    // attribute that must still exempt despite what precedes `test`, and one on each side of the
    // word that must not. They matter to a *port* more than to the original, because the obvious
    // Rust spelling of "contains test" exempts all three — and a rule that exempts too much
    // reports nothing, which reads exactly like clean code. `_` being a word character in
    // JavaScript's `\w` is what decides `my_test`, and it decides it the same way here.
    //
    // **Both edges, and the trailing one was missing.** With `mentions_test` mutated to drop its
    // trailing boundary check, every case in this table still passed: `tokio::test` and `my_test`
    // between them pin only what comes *before* the word. `#[testing]` is what makes the other
    // half fail.
    Case {
        name: "a_test_attribute_behind_a_path_still_exempts",
        source: "#[tokio::test]\nasync fn works() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::Accepts,
    },
    Case {
        name: "an_attribute_that_merely_contains_test_does_not_exempt",
        source: "#[my_test]\nfn works() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(3, 13)]),
    },
    Case {
        name: "an_attribute_that_merely_starts_with_test_does_not_exempt",
        source: "#[testing]\nfn works() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(3, 13)]),
    },
    Case {
        name: "a_cfg_test_module_passes",
        source: "#[cfg(test)]\nmod tests {\n    fn helper() {\n        let c = load().unwrap();\n    }\n}\n",
        options: None,
        expected: Expected::Accepts,
    },
    Case {
        name: "an_unrelated_method_passes",
        source: "fn f() {\n    let c = load().clone();\n}\n",
        options: None,
        expected: Expected::Accepts,
    },
    // The rule's known limitation, asserted rather than left to be discovered: a builder method
    // genuinely named `expect` cannot be told apart from `Result::expect` without type
    // information, which lanekeep deliberately does not have.
    Case {
        name: "a_method_named_expect_on_a_mock_is_still_reported",
        source: "fn f() {\n    mock.expect(\"calls\");\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(2, 5)]),
    },
    // The option the rule's own JSDoc documents — `noUnwrap({ allow: ['src/main.rs'] })` — and
    // which reached the handler as `undefined` on every run, because the default export was a
    // plain object rather than a factory. `src/main.rs` is the usual one.
    Case {
        name: "an_allowed_path_passes",
        source: "fn f() {\n    let c = load().unwrap();\n}\n",
        options: Some(r#"{"allow": ["subject/input.rs"]}"#),
        expected: Expected::Accepts,
    },
    // The other half, and the one that makes the case above mean something: an `allow` that
    // exempted everything would pass it just as well.
    Case {
        name: "a_path_outside_the_allow_list_is_still_reported",
        source: "fn f() {\n    let c = load().unwrap();\n}\n",
        options: Some(r#"{"allow": ["src/main.rs"]}"#),
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    Case {
        name: "a_wildcard_in_an_allowed_path_matches",
        source: "fn f() {\n    let c = load().unwrap();\n}\n",
        options: Some(r#"{"allow": ["subject/*.rs"]}"#),
        expected: Expected::Accepts,
    },
    // No message was asserted anywhere for this rule. `reports_at` pins positions only, so a
    // template that interpolated the wrong capture would read fine and go unnoticed.
    Case {
        name: "the_message_names_the_method_that_was_called",
        source: "fn f() {\n    let c = load().expect(\"boom\");\n}\n",
        options: None,
        expected: Expected::ReportsMessages(&[
            "`expect()` aborts the process where the caller wanted an error it could handle",
        ]),
    },
    // The regression this rule shipped with on its first draft, in both directions.
    // `fileContains` is an *and*, so gating on `['unwrap', 'expect']` rejected every file that
    // used one without the other — which is nearly all of them. Nothing failed; the rule simply
    // reported nothing, which reads exactly like a codebase that has no unwraps in it.
    //
    // Two cases rather than one test asserting twice: a table entry is one source and one
    // expectation, and splitting them is what keeps a failure naming the direction that broke.
    Case {
        name: "a_file_with_unwrap_and_no_expect_is_still_checked",
        source: "fn f() {\n    let c = load().unwrap();\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(2, 13)]),
    },
    Case {
        name: "a_file_with_expect_and_no_unwrap_is_still_checked",
        source: "fn g() {\n    let c = load().expect(\"m\");\n}\n",
        options: None,
        expected: Expected::ReportsAt(&[(2, 13)]),
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
/// **Measured rather than supposed.** With the component's `metadata()` returning
/// `id: "lanekeep/no-unwrapp"`, `severity: "warn"` and a remediation reading
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
/// `cargo test`. Measured before this: **29.6 seconds** for the component arm run in sequence,
/// against 0.4 for the TypeScript arm it then had, in a recipe that runs on every commit.
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

/// The rule: a WebAssembly component built from `rust-rules/no-unwrap/`.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON, and why the bare case is not
/// "no call" but a call with `null`.
///
/// Both shapes have to work and for a while only one of them did: `lanekeep init` writes a bare
/// `"lanekeep/no-unwrap"` into a Rust project's config, while the usage the rule documents
/// configures it. That is why the table carries cases of each kind rather than only the
/// configured ones.
fn component(case: &Case) -> RuleTester {
    let bytes = lanekeep_rules::component("no-unwrap").expect("the component ships");
    match case.options {
        None => RuleTester::for_component("no-unwrap", bytes, "rs"),
        Some(options) => RuleTester::for_component_configured("no-unwrap", bytes, "rs", options),
    }
    .expect("builds")
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}
