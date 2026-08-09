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

/// A tester for the rule as its own documentation spells it: `noUnwrap({ allow: [...] })`.
///
/// Every case above reaches the rule *bare*, which is the shape `lanekeep init` writes into a
/// Rust project's config. Both shapes have to work, and only one of them was covered.
fn configured(options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-unwrap").expect("the rule ships");
    RuleTester::configured_with_extension("no-unwrap", source, "rs", options).expect("builds")
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
fn an_allowed_path_passes() {
    // The option the rule's own JSDoc documents — `noUnwrap({ allow: ['src/main.rs'] })` —
    // and which reached the handler as `undefined` on every run, because the default export
    // was a plain object rather than a factory. `src/main.rs` is the usual one.
    configured("{ allow: ['subject/input.rs'] }")
        .accepts("fn f() {\n    let c = load().unwrap();\n}\n")
        .expect("an allowed path is exempt");
}

#[test]
fn a_path_outside_the_allow_list_is_still_reported() {
    // The other half, and the one that makes the test above mean something: an `allow` that
    // exempted everything would pass it just as well.
    configured("{ allow: ['src/main.rs'] }")
        .reports_at("fn f() {\n    let c = load().unwrap();\n}\n", &[(2, 13)])
        .expect("a path the list does not name is still checked");
}

#[test]
fn a_wildcard_in_an_allowed_path_matches() {
    configured("{ allow: ['subject/*.rs'] }")
        .accepts("fn f() {\n    let c = load().unwrap();\n}\n")
        .expect("`*` spans a path segment");
}

#[test]
fn the_message_names_the_method_that_was_called() {
    // No message was asserted anywhere for this rule. `reports_at` pins positions only, so a
    // template that interpolated the wrong capture would read fine and go unnoticed.
    tester()
        .reports_messages(
            "fn f() {\n    let c = load().expect(\"boom\");\n}\n",
            &["`expect()` aborts the process where the caller wanted an error it could handle"],
        )
        .expect("the message names the method the call site used");
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
