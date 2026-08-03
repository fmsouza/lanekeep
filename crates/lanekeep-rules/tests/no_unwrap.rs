//! `lanekeep/no-unwrap`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-unwrap").expect("the rule ships");
    RuleTester::with_extension("no-unwrap", source, "rs").expect("builds")
}

#[test]
fn the_question_mark_passes() {
    tester()
        .accepts("fn f() -> Result<u8, E> {\n    let c = load()?;\n    Ok(c)\n}\n")
        .expect("propagating is the shape the rule steers towards");
}

#[test]
fn unwrap_is_reported() {
    tester()
        .reports_at("fn f() {\n    let c = load().unwrap();\n}\n", &[(2, 13)])
        .expect("unwrap aborts where the caller wanted an error");
}

#[test]
fn expect_is_reported_too() {
    tester()
        .reports_at(
            "fn f() {\n    let c = load().expect(\"boom\");\n}\n",
            &[(2, 13)],
        )
        .expect("expect panics just the same, with a nicer message");
}

#[test]
fn a_test_function_passes() {
    // In a test, panicking *is* the failure mechanism. Reporting here would mean either a
    // rule nobody can turn on or a suppression on every assertion.
    tester()
        .accepts("#[test]\nfn works() {\n    let c = load().unwrap();\n}\n")
        .expect("a #[test] function may unwrap");
}

#[test]
fn a_cfg_test_module_passes() {
    tester()
        .accepts(
            "#[cfg(test)]\nmod tests {\n    fn helper() {\n        let c = load().unwrap();\n    }\n}\n",
        )
        .expect("a #[cfg(test)] module may unwrap");
}

#[test]
fn an_unrelated_method_passes() {
    tester()
        .accepts("fn f() {\n    let c = load().clone();\n}\n")
        .expect("only unwrap and expect are the problem");
}

#[test]
fn a_method_named_expect_on_a_mock_is_still_reported() {
    // The rule's known limitation, asserted rather than left to be discovered: a builder
    // method genuinely named `expect` cannot be told apart from `Result::expect` without
    // type information, which lanekeep deliberately does not have.
    tester()
        .reports_at("fn f() {\n    mock.expect(\"calls\");\n}\n", &[(2, 5)])
        .expect("indistinguishable from Result::expect without types");
}

#[test]
fn a_file_containing_only_one_of_the_two_words_is_still_checked() {
    // The regression this rule shipped with on its first draft. `fileContains` is an *and*,
    // so gating on `['unwrap', 'expect']` rejected every file that used one without the
    // other — which is nearly all of them. Nothing failed; the rule simply reported nothing,
    // which reads exactly like a codebase that has no unwraps in it.
    tester()
        .reports_at("fn f() {\n    let c = load().unwrap();\n}\n", &[(2, 13)])
        .expect("a file with `unwrap` and no `expect` must still be checked");
    tester()
        .reports_at(
            "fn g() {\n    let c = load().expect(\"m\");\n}\n",
            &[(2, 13)],
        )
        .expect("and the other way round");
}
