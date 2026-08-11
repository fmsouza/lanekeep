//! `lanekeep/no-package-init`, run through the real engine.
//!
//! # Why the cases are a table
//!
//! Every case is a [`Case`] in [`CASES`], and a test function runs the whole table against a
//! tester it is handed. `crates/lanekeep-rules/tests/no_unwrap.rs` is where that arrangement was
//! worked out, and its module doc is the argument for it — including the sentence that the
//! indirection stays "because the next migration needs it". This file is that migration: the rule
//! was a TypeScript module and is a component built from Go now, and the two ran side by side
//! against every case in this table until the swap landed.
//!
//! Two independently written test files could each look sensible and still assert different
//! things, and the difference would be invisible: a case only one of them has is a case the other
//! is not held to. A table cannot express that. So the Go implementation was held to the
//! expectations the TypeScript one was already held to, rather than to a second file that looks
//! reasonable.
//!
//! **There is one arm again, and the table is what earned the right to say so.** The indirection
//! stays for the reason it was kept the last time — a case written against a tester rather than
//! against an engine is the shape that survives a migration, and the two Python built-ins have
//! not had theirs yet.
//!
//! So a case belongs here rather than in a `#[test]` of its own, and one that could only be
//! written for a single arm is worth stopping over rather than working around.
//!
//! # The violation's message is pinned elsewhere, which is why [`Expected`] has no variant for it
//!
//! `no_unwrap.rs` carries a `ReportsMessages` case because nothing anywhere asserted that rule's
//! message, and [`assert_identity`] cannot reach it — the id, the severity and the remediation
//! come from the card, and the message comes from the `report` call in the handler. This rule's
//! message *is* asserted, exactly, against the committed component:
//! `crates/lanekeep-wasm/tests/world_shape.rs`'s `the_go_builtins_component_answers_the_world_it_targets`
//! reads what the guest reports through the real ABI. A second copy here would be a second place
//! to edit and no new claim, so the variant is absent rather than unused. Add it back the moment
//! a case needs it.
//!
//! # One tester for the whole table, rather than one per case
//!
//! `no_unwrap.rs` builds a tester per case and runs them on a thread each, because its cases vary
//! the rule's options and a tester's config is fixed when it is built. This rule takes none, so
//! one tester serves every case — and that is what the component arm wants anyway: the first
//! `run` on a built-in tester is where the component is compiled, and a tester per case would pay
//! that once per row. A tester cannot be shared *across* threads, since [`RuleTester::run`]
//! rewrites one subject directory, so the parallelism and the single tester are alternatives
//! rather than a choice worth having both ways.

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

/// The rule, as a config names it and as [`lanekeep_rules::component`] is asked for it.
const RULE: &str = "no-package-init";

/// The rule id every violation carries.
const RULE_ID: &str = "lanekeep/no-package-init";

/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// The remediation every violation carries, from the rule's card.
const REMEDIATION: &str = "move the work into an explicit constructor or a call from main, so \
                           the order it happens in is written down";

/// One case, written once and run against every engine this rule has an arm for.
///
/// One today, the component. The shape is what let the same expectations hold the TypeScript
/// implementation that preceded it, and is the seam the next arm attaches at.
struct Case {
    /// What it is called in a failure report. Named as the `#[test]` function it replaces was,
    /// so a failure here is greppable against the history that motivated the case.
    name: &'static str,
    /// The subject file, written as `subject/input.go`.
    source: &'static str,
    /// What the rule must report.
    expected: Expected,
}

/// Every case, run against every arm below.
///
/// No case carries options, because this rule takes none — there is no `options` column here for
/// the same reason [`RuleTester::for_built_in`] rather than its `_configured` sibling is what the
/// arm below builds.
const CASES: &[Case] = &[
    // Something main can call in an order it chooses.
    Case {
        name: "an_ordinary_function_passes",
        source: "package a\n\nfunc Setup() error { return nil }\n",
        expected: Expected::Accepts,
    },
    // `init` runs at import time in an order nothing states.
    Case {
        name: "a_package_init_is_reported",
        source: "package a\n\nfunc init() {\n\tregister()\n}\n",
        expected: Expected::ReportsAt(&[(3, 1)]),
    },
    // Go permits several, which is exactly what makes the ordering hard to reason about — two
    // inits in one file are two problems, and a handler stopping at the first would pass every
    // other case in this table.
    Case {
        name: "every_init_in_a_file_is_reported",
        source: "package a\n\nfunc init() {\n\ta()\n}\n\nfunc init() {\n\tb()\n}\n",
        expected: Expected::ReportsAt(&[(3, 1), (7, 1)]),
    },
    // The rule matches a declaration, not the text `init`, so a variable of that name is
    // untouched — and the `fileContains` gate deliberately admits this file.
    Case {
        name: "a_local_variable_named_init_passes",
        source: "package a\n\nfunc f() {\n\tinit := 1\n\t_ = init\n}\n",
        expected: Expected::Accepts,
    },
    // A method is a `method_declaration`, which the query cannot match. It is also not called
    // implicitly, so it does not have the property the rule objects to.
    Case {
        name: "a_method_named_init_passes",
        source: "package a\n\ntype T struct{}\n\nfunc (t *T) init() {}\n",
        expected: Expected::Accepts,
    },
    // A func literal assigned to a variable runs when it is called, like anything else.
    Case {
        name: "a_function_literal_named_by_a_variable_passes",
        source: "package a\n\nfunc f() {\n\tinit := func() {}\n\tinit()\n}\n",
        expected: Expected::Accepts,
    },
];

/// Assert one case against the tester it is run on.
///
/// One `run` rather than one per assertion. The harness's `accepts`/`reports_at`/
/// `reports_messages` each run the engine themselves and hand back only a verdict, and a case
/// needs both the verdict and the violations behind it — so the comparison is done here, against
/// one set of results.
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
/// Measured rather than supposed, on the rule this arrangement was worked out for: with a
/// component answering a mistyped id, a different severity and a rewritten remediation, every
/// position and message case still passed. A table asserting only positions and messages says
/// nothing about half of what a violation is, in a migration whose whole claim is that the two
/// implementations report identically.
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
/// One `#[test]` per engine rather than one per case, because the table is the unit: a case that
/// exists for one engine and not the other is the thing this file is arranged to make
/// unrepresentable. Collecting the failures is what keeps that from costing anything — a
/// migration that gets three cases wrong says so once, rather than three runs in a row.
///
/// One engine today. The parameter is not vestigial: it is the seam a second arm is added at,
/// and this file has had two arms before.
fn assert_every_case(tester: &RuleTester) {
    let mut failures = String::new();
    for case in CASES {
        if let Err(error) = assert_case(tester, case) {
            let _ = write!(failures, "\n--- {} ---\n{error}\n", case.name);
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// The rule as it ships: the Go component, reached the way a config reaches it.
///
/// # `for_built_in` rather than `for_component`, and it is not a preference
///
/// [`RuleTester::for_component`] writes an artifact to a path, and **a path reference contributes
/// every rule the artifact hosts**. `go-builtins.wasm` hosts two, so a path-backed tester would
/// run `no-context-in-struct` over these subjects as well — and whether that shows up depends on
/// nothing but whether a subject happens to trip the other rule's `fileContains` gate, which is
/// luck rather than a property. Naming the *specifier* is how a config says which rule it means:
/// resolution goes through the embedded table, which carries the rule's index, and the engine is
/// handed one rule.
///
/// # Asking the lookup is what makes this an arm rather than a second copy of the other one
///
/// `for_built_in` writes `"lanekeep/no-package-init"` into a `lanekeep.json` and nothing else,
/// and `lanekeep-config`'s `classify` falls back to the *module* for a name the component lookup
/// does not answer — a config naming a built-in cannot say which kind it is, deliberately. Today
/// that fallback cannot fire here, because `RuleTester` never calls `RuleRoot::with_builtins`, so
/// a built-in tester has no source lookup to fall back *to*: before this rule was wired into
/// `COMPONENT_RULES` every case failed with `cannot find module 'lanekeep/no-package-init'`.
///
/// That is a property of another crate's wiring rather than of this file, and what it stands
/// between is proving the component right and running the module twice under a name that says
/// otherwise. So it is asserted here, where it costs a line, rather than relied on from a
/// distance.
fn component() -> RuleTester {
    lanekeep_rules::component(RULE).expect("the rule ships as a component rather than as a module");
    RuleTester::for_built_in(RULE, "go", lanekeep_rules::component).expect("builds")
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(&component());
}
