//! `lanekeep/no-assertionless-test`, run through the real engine.
//!
//! One rule, four language families: what each language's cases assert is the pair the rule
//! is made of — its test-definition detection and its assertion vocabulary — plus the
//! exemptions that are correctness rather than convenience (`t.Skip`, `#[should_panic]`).
//! The subject file the harness writes is at `subject/input.<ext>`, which is what the
//! `tests` globs here are written against.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester_for(extension: &str, options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-assertionless-test").expect("the rule ships");
    RuleTester::configured_with_extension("no-assertionless-test", source, extension, options)
        .expect("builds")
        .with_builtins(lanekeep_rules::source)
}

// --- typescript ---------------------------------------------------------------------------

#[test]
fn a_typescript_test_without_an_assertion_is_reported() {
    tester_for("ts", "{}")
        .reports_at("it('adds', () => {\n  add(1, 2)\n})\n", &[(1, 1)])
        .expect("a test body that checks nothing is the failure this rule exists for");
}

#[test]
fn a_typescript_test_with_an_expect_is_fine() {
    tester_for("ts", "{}")
        .accepts("it('adds', () => {\n  expect(add(1, 2)).toBe(3)\n})\n")
        .expect("expect() is the default vocabulary");
}

#[test]
fn the_test_name_and_the_only_variant_are_covered() {
    tester_for("ts", "{}")
        .reports_at(
            "test('adds', function () {\n  add(1, 2)\n})\nit.only('subtracts', () => {\n  sub(3, 1)\n})\n",
            &[(1, 1), (4, 1)],
        )
        .expect("`test(...)` and `it.only(...)` are tests too");
}

#[test]
fn an_ordinary_function_call_is_not_a_test() {
    tester_for("ts", "{}")
        .accepts("setup('adds', () => {\n  add(1, 2)\n})\n")
        .expect("only the test vocabulary defines a test");
}

#[test]
fn a_configured_assertion_name_is_honored() {
    // The ignored-options trap: an ignored vocabulary only ever adds violations, so the
    // accepting direction is what proves the option reached the rule.
    tester_for("ts", "{ assertions: { typescript: ['verify'] } }")
        .accepts("it('adds', () => {\n  verify(add(1, 2))\n})\n")
        .expect("the configured vocabulary counts as asserting");
}

#[test]
fn an_allowed_helper_counts_in_every_language() {
    tester_for("ts", "{ allowHelpers: ['expectValidResponse'] }")
        .accepts("it('responds', () => {\n  expectValidResponse(call())\n})\n")
        .expect("a helper the config vouches for counts as asserting");
}

// --- python -------------------------------------------------------------------------------

#[test]
fn a_python_test_without_an_assertion_is_reported() {
    tester_for("py", "{}")
        .reports_at("def test_add():\n    helper()\n", &[(1, 1)])
        .expect("a test_ function that checks nothing is reported at its definition");
}

#[test]
fn a_python_assert_statement_is_an_assertion() {
    // `assert` is a *statement* in python, not a call — the vocabulary has to be
    // node-shaped there, not only name-shaped.
    tester_for("py", "{}")
        .accepts("def test_add():\n    assert add(1, 2) == 3\n")
        .expect("the assert statement is the language's own assertion form");
}

#[test]
fn python_unittest_methods_and_pytest_raises_are_assertions() {
    tester_for("py", "{}")
        .accepts(
            "class TestAdd(TestCase):\n    def test_add(self):\n        self.assertEqual(add(1, 2), 3)\n\ndef test_raises():\n    with pytest.raises(ValueError):\n        add(None, None)\n",
        )
        .expect("`self.assert*` and `pytest.raises` are the default vocabulary");
}

#[test]
fn an_ordinary_python_function_is_not_a_test() {
    tester_for("py", "{}")
        .accepts("def helper():\n    do_things()\n")
        .expect("a non-test function asserting nothing is fine");
}

#[test]
fn the_tests_globs_gate_where_the_rule_looks() {
    // Both directions, or a gate that is ignored looks exactly like a gate that matches
    // everything.
    let inside = tester_for("py", "{ tests: ['subject/*'] }");
    inside
        .reports_at("def test_add():\n    helper()\n", &[(1, 1)])
        .expect("the subject is inside the tests globs");

    let outside = tester_for("py", "{ tests: ['elsewhere/*'] }");
    outside
        .accepts("def test_add():\n    helper()\n")
        .expect("the subject is outside the tests globs, so the rule never looks");
}

// --- go -----------------------------------------------------------------------------------

#[test]
fn a_go_test_without_an_assertion_is_reported() {
    tester_for("go", "{}")
        .reports_at(
            "package main\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\thelper()\n}\n",
            &[(5, 1)],
        )
        .expect("a Test function that checks nothing is reported at its definition");
}

#[test]
fn go_t_error_and_testify_are_assertions() {
    tester_for("go", "{}")
        .accepts(
            "package main\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif add(1, 2) != 3 {\n\t\tt.Errorf(\"wrong\")\n\t}\n}\n\nfunc TestSub(t *testing.T) {\n\trequire.NoError(t, sub())\n}\n",
        )
        .expect("`t.Error*` and `require.*` are the default vocabulary");
}

#[test]
fn a_go_function_without_the_testing_parameter_is_not_a_test() {
    // `TestHelper(data string)` is a name collision, not a test — the `*testing.T`
    // parameter is what makes go's convention a convention.
    tester_for("go", "{}")
        .accepts("package main\n\nfunc TestHelper(data string) {\n\thelper(data)\n}\n")
        .expect("the Test prefix alone does not make a test");
}

#[test]
fn a_skipped_go_test_is_exempt() {
    // A skipped test legitimately asserts nothing; reporting it would punish the honest
    // spelling of "this cannot run here".
    tester_for("go", "{}")
        .accepts(
            "package main\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tt.Skip(\"needs a database\")\n}\n",
        )
        .expect("t.Skip is an exemption, not an assertion");
}

// --- rust ---------------------------------------------------------------------------------

#[test]
fn a_rust_test_without_an_assertion_is_reported() {
    tester_for("rs", "{}")
        .reports_at("#[test]\nfn adds() {\n    helper();\n}\n", &[(2, 1)])
        .expect("a #[test] function that checks nothing is reported at its definition");
}

#[test]
fn rust_assert_macros_are_assertions() {
    tester_for("rs", "{}")
        .accepts("#[test]\nfn adds() {\n    assert_eq!(add(1, 2), 3);\n}\n")
        .expect("assert!/assert_eq!/assert_ne! are the default vocabulary");
}

#[test]
fn a_should_panic_rust_test_is_exempt() {
    tester_for("rs", "{}")
        .accepts("#[test]\n#[should_panic]\nfn overflows() {\n    add(i32::MAX, 1);\n}\n")
        .expect("the panic is the assertion");
}

#[test]
fn a_function_without_the_test_attribute_is_not_a_test() {
    // `#[cfg(test)]` gates compilation and `#[inline]` is unrelated; neither makes the
    // function a test, so neither may cause a report.
    tester_for("rs", "{}")
        .accepts(
            "#[cfg(test)]\nfn helper() {\n    do_things();\n}\n\nfn plain() {\n    more();\n}\n",
        )
        .expect("only the test attribute defines a test");
}

#[test]
fn an_attribute_path_ending_in_test_is_a_test() {
    // `#[tokio::test]` and friends: the attribute is a path whose last segment is `test`.
    tester_for("rs", "{}")
        .reports_at(
            "#[tokio::test]\nasync fn responds() {\n    call().await;\n}\n",
            &[(2, 1)],
        )
        .expect("a ::test attribute defines a test exactly as #[test] does");
}
