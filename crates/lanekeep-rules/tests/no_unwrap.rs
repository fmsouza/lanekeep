//! `lanekeep/no-unwrap`, run through the real engine.
//!
//! # Why the cases are a table
//!
//! Every case is a [`Case`] in [`CASES`], and a test function runs the whole table against a
//! tester it is handed. That indirection buys nothing today, with one implementation of the
//! rule and one arm to run it through, and it is what makes a second implementation testable
//! against *the same* expectations rather than against a second file that looks reasonable.
//!
//! Two independently written test files could each look sensible and still assert different
//! things, and the difference would be invisible: a case only one of them has is a case the
//! other is not held to. A table cannot express that.
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

use lanekeep_testkit::{RuleTester, TestError};

/// What a case asserts about what the rule reported.
enum Expected {
    /// Nothing at all.
    Accepts,
    /// Exactly these one-based positions, in order.
    ReportsAt(&'static [(u32, u32)]),
    /// Exactly these messages, in order.
    ReportsMessages(&'static [&'static str]),
}

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
    // The exemption is `/\btest\b/` over the attribute's text, and these are the two edges of
    // that `\b`. They matter to a *port* more than to the original: the obvious Rust spelling of
    // "contains test" exempts `#[my_test]`, and a rule that exempts too much reports nothing and
    // reads exactly like clean code. `_` being a word character is what decides the second one,
    // in JavaScript and here alike.
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
fn assert_case(tester: &RuleTester, case: &Case) -> Result<(), TestError> {
    match case.expected {
        Expected::Accepts => tester.accepts(case.source),
        Expected::ReportsAt(positions) => tester.reports_at(case.source, positions),
        Expected::ReportsMessages(messages) => tester.reports_messages(case.source, messages),
    }
}

/// Run the whole table, reporting every failure rather than only the first.
///
/// One `#[test]` per engine rather than one per case, because the table is the unit: a case
/// that exists for one engine and not the other is the thing this file is arranged to make
/// unrepresentable. Collecting the failures is what keeps that from costing anything — a
/// migration that gets three cases wrong says so once, rather than three runs in a row.
///
/// # A thread per case, which nextest would otherwise have given for free
///
/// Every case builds its own throwaway project and runs the real engine over it, and for the
/// component arm that means compiling the rule twice — once when the config asks it what it is,
/// once when the engine prepares — with a Cranelift that is itself an unoptimized build under
/// `cargo test`. Measured before this: **29.6 seconds** for the component arm run in sequence,
/// against 0.4 for the TypeScript one, in a recipe that runs on every commit.
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

/// The rule as authored: a TypeScript module, evaluated in the sandbox.
///
/// `configured_with_extension` for a case with options and `with_extension` for one without,
/// because that distinction is the rule's own: `lanekeep init` writes a bare
/// `"lanekeep/no-unwrap"` into a Rust project's config, which `lanekeep-config` renders as the
/// imported binding itself, while the usage the rule documents calls it. Both shapes have to
/// work, and for a while only one of them did.
fn typescript(case: &Case) -> RuleTester {
    let source = lanekeep_rules::source("no-unwrap").expect("the rule ships");
    match case.options {
        None => RuleTester::with_extension("no-unwrap", source, "rs"),
        Some(options) => RuleTester::configured_with_extension("no-unwrap", source, "rs", options),
    }
    .expect("builds")
}

/// The same rule, migrated: a WebAssembly component built from `rust-rules/no-unwrap/`.
///
/// The options reach it as data rather than as source. A component cannot close over a
/// host-supplied value the way a JavaScript factory does, so `configure(options-json)` is where
/// they arrive — which is why the table's option strings are JSON, and why the bare case is not
/// "no call" but a call with `null`.
fn component(case: &Case) -> RuleTester {
    let bytes = lanekeep_rules::component("no-unwrap").expect("the component ships");
    match case.options {
        None => RuleTester::for_component("no-unwrap", bytes, "rs"),
        Some(options) => RuleTester::for_component_configured("no-unwrap", bytes, "rs", options),
    }
    .expect("builds")
}

#[test]
fn the_typescript_rule_satisfies_every_case() {
    assert_every_case(typescript);
}

#[test]
fn the_component_rule_satisfies_every_case() {
    assert_every_case(component);
}
