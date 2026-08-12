//! `local/host-api-matches-types`, run through the real engine.
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
//! # This rule reads two files
//!
//! The subject is the host file, written by [`RuleTester::run`] as `subject/input.rs`. The
//! types file is read through `ctx.read_file`, which is confined to the project root — so the
//! tester's own directory is the root, and the types fixture is written beside the subject with
//! [`RuleTester::write_fixture`]. Every case carries its own `types` fixture, and the two
//! real-file cases read the actual `host.rs` and `index.d.ts` with `include_str!` so a moved or
//! renamed file fails to compile rather than silently checking nothing.

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
    /// Exactly these messages, in order. Every finding is reported at the file root, so the
    /// position says nothing and the message is the assertion.
    ReportsMessages(&'static [&'static str]),
    /// At least this many violations. The real-host floor test asserts the rule actually
    /// matched the host's `object.set(` calls rather than silently no-op-ing.
    AtLeast(usize),
}

/// The rule id every violation carries.
const RULE_ID: &str = "local/host-api-matches-types";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str = "register the function and declare it in the same change — and bump `host_api_version`, which is a cache key input";

/// The options every case shares: the subject is the host file, and the types fixture is the
/// other file the rule reads.
const OPTIONS: &str = r#"{"hostPath": "subject/input.rs", "typesPath": "types.d.ts"}"#;

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
    /// The types fixture, written as `types.d.ts`.
    types: &'static str,
    /// What the rule must report.
    expected: Expected,
}

/// Every case, run against every arm below.
///
/// Migrated from `crates/lanekeep-cli/tests/selfcheck.rs`'s `host-api` group. The harness
/// cases there — the `RuleTester::configured_with_extension` calls that built a tester — were
/// scaffolding and are not migrated; the seven assertions they served are.
const CASES: &[Case] = &[
    // A host function nobody can find is as good as absent.
    Case {
        name: "a_registration_missing_from_the_types_is_reported",
        source: "fn r() {\n    object.set(\"text\", 1)?;\n    object.set(\"newThing\", 2)?;\n}\n",
        types: "export interface RuleContext {\n  text(node: Node): string\n}\n",
        expected: Expected::ReportsMessages(&[
            "host.rs registers `newThing`, which packages/lanekeep/index.d.ts does not declare — it works but nobody can find it",
        ]),
    },
    // A declared method that throws at run time is worse than no types.
    Case {
        name: "a_type_with_no_registration_is_reported",
        source: "fn r() {\n    object.set(\"text\", 1)?;\n}\n",
        types: "export interface RuleContext {\n  text(node: Node): string\n  invented(node: Node): string\n}\n",
        expected: Expected::ReportsMessages(&[
            "index.d.ts declares `invented`, which host.rs does not register — autocomplete for a method that throws",
        ]),
    },
    // Registered and declared is the state this rule protects.
    Case {
        name: "a_matching_pair_passes",
        source: "fn r() {\n    object.set(\"text\", 1)?;\n}\n",
        types: "export interface RuleContext {\n  text(node: Node): string\n}\n",
        expected: Expected::Accepts,
    },
    // The historical bug this guards, empirically: an early version stripped Rust string
    // prefixes with `/^["r#]+/`, greedy across `"` and `r` together, which ate the leading `r`
    // of names like `root`, `report` and `readFile` and invented four mismatches against the
    // real repository. None of this file's other fixtures register a name starting with `r`,
    // so none of them would have caught it — this one registers and declares `root` and
    // requires the pair to reconcile cleanly rather than becoming `root` vs. `oot`.
    Case {
        name: "a_registered_name_starting_with_r_is_recognized",
        source: "fn r() {\n    object.set(\"root\", 1)?;\n}\n",
        types: "export interface RuleContext {\n  root(node: Node): string\n}\n",
        expected: Expected::Accepts,
    },
    // `host.rs`'s own `#[cfg(test)] mod tests { ... }` never calls `object.set("literal",
    // ...)` today, so nothing in the real repository exercises this exclusion — this fixture
    // stands in for what a future test-only registration inside `host.rs` would look like.
    Case {
        name: "a_registration_inside_test_code_is_ignored",
        source: "#[cfg(test)]\nmod tests {\n    fn t() {\n        object.set(\"fakeThing\", 1)?;\n    }\n}\n",
        types: "export interface RuleContext {\n}\n",
        expected: Expected::Accepts,
    },
    // The deleted host_types.rs used include_str! against the real host.rs — a moved or
    // renamed file failed to COMPILE. This rule's `hostPath` is a runtime string instead, and
    // `check` returns early on `ctx.file_path() != host_path`, so without an anchor like this
    // one a rename would make the rule silently check nothing on every file while `just
    // lanekeep` stayed green. Mirrors what `the_rule_still_matches_the_real_host_source` does
    // for the floor — and, on its own, would not be enough: see that case for why a second
    // assertion is needed alongside this one.
    Case {
        name: "the_real_host_and_types_reconcile",
        source: include_str!("../../lanekeep-js/src/host.rs"),
        types: include_str!("../../../packages/lanekeep/index.d.ts"),
        expected: Expected::Accepts,
    },
    // Reconciling cleanly, above, is not enough on its own — a rule that returned early for
    // every file would also reconcile cleanly, vacuously. An empty `RuleContext` here proves
    // `check` actually read and matched the real host.rs's `object.set(` calls rather than
    // silently no-op-ing. There are roughly 31 `object.set(` call sites in host.rs; this floor
    // leaves generous headroom below the real, deduplicated count so one or two future
    // registrations do not make the test flaky.
    Case {
        name: "the_rule_still_matches_the_real_host_source",
        source: include_str!("../../lanekeep-js/src/host.rs"),
        types: "export interface RuleContext {\n}\n",
        expected: Expected::AtLeast(15),
    },
];

/// Assert one case against a tester built for it.
///
/// One `run` rather than one per assertion. The harness's `accepts`/`reports_messages` each run
/// the engine themselves and hand back only a verdict, and a case needs both the verdict and
/// the violations behind it — so the comparison is done here, against one set of results. For
/// the component arm that also halves the work: a run there compiles the rule twice, and two
/// runs per case would be four.
fn assert_case(tester: &RuleTester, case: &Case) -> Result<(), TestError> {
    tester
        .write_fixture("types.d.ts", case.types)
        .expect("the types fixture is written");
    let violations = tester.run(case.source)?;
    assert_identity(&violations, case)?;

    match case.expected {
        Expected::Accepts if violations.is_empty() => Ok(()),
        Expected::Accepts => Err(mismatch(
            case,
            "expected no violations",
            &rendered(&violations),
        )),
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
        Expected::AtLeast(floor) if violations.len() >= floor => Ok(()),
        Expected::AtLeast(floor) => Err(mismatch(
            case,
            &format!("at least {floor} violations"),
            &format!("{}", violations.len()),
        )),
    }
}

/// Hold every violation to the three fields no [`Expected`] variant pins.
///
/// **Measured rather than supposed.** With the component's `metadata()` returning
/// `id: "local/host-api-matches-types"`, `severity: "warn"` and a remediation reading
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

/// The rule: a WebAssembly component built from `rust-rules/host-api-matches-types/`.
///
/// A **project rule** (`local/` id), so it is not embedded in the binary the way a built-in is.
/// The tester reads the committed artifact `just rust-rules` produced from disk, and a path
/// reference contributes every rule the artifact hosts — which is one, so the index is
/// discarded and the whole component is the rule.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON. Every case shares the same
/// options, because the rule only runs on the host file and the types fixture is always the
/// other file it reads.
fn component(_case: &Case) -> RuleTester {
    const COMPONENT: &[u8] = include_bytes!("../components/host-api-matches-types.wasm");
    RuleTester::for_component_configured("host-api-matches-types", COMPONENT, "rs", OPTIONS)
        .expect("builds")
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}
