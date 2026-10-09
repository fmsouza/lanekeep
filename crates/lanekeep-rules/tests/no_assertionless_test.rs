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
fn playwright_hooks_steps_and_describe_are_not_tests() {
    // #287: every `test.<member>(...)` used to be a test, because the query captured only the
    // member's object. Hooks and steps are not expected to assert, and a describe holding an
    // asserting test is not itself a test.
    tester_for("ts", "{}")
        .accepts(
            "test.beforeEach(async ({ page }) => {\n  await page.goto('/')\n})\n\ntest.afterAll(async () => {\n  await cleanup()\n})\n\ntest.describe('group', () => {\n  test.beforeAll(() => {\n    seed()\n  })\n  test('works', async ({ page }) => {\n    await test.step('open', async () => {\n      await page.goto('/')\n    })\n    await expect(page).toHaveTitle('x')\n  })\n})\n",
        )
        .expect("hooks, steps and describe blocks are not tests and need not assert");
}

#[test]
fn a_describe_holding_no_assertion_is_not_a_test() {
    // The other half of #287's second repro: the enclosing describe was reported too when
    // nothing inside it asserted. Whether a describe holds a test is not this rule's business.
    tester_for("ts", "{}")
        .accepts(
            "test.describe('group', () => {\n  test.beforeEach(() => {\n    seed()\n  })\n  test.afterEach(() => {\n    reset()\n  })\n})\n",
        )
        .expect("a describe block is a grouping, not a test");
}

#[test]
fn the_test_declaring_modifiers_are_still_tests() {
    // The allow-list is what fixes #287, so it must not narrow past the modifier forms that do
    // declare a test: jest/vitest `concurrent`, Playwright `fixme`, jest `failing`.
    tester_for("ts", "{}")
        .reports_at(
            "test.skip('a', () => {\n  a()\n})\ntest.concurrent('b', async () => {\n  b()\n})\ntest.fixme('c', () => {\n  c()\n})\nit.failing('d', () => {\n  d()\n})\n",
            &[(1, 1), (4, 1), (7, 1), (10, 1)],
        )
        .expect("each modifier form declares a test, so an empty one is reported");
}

#[test]
fn table_driven_tests_are_tests() {
    // #288's repro: only the plain `it` was reported. `it.each(table)` returns the function
    // the test is declared with, so the callback is in the *outer* call of a call.
    tester_for("ts", "{}")
        .reports_at(
            "it('plain', () => {\n  void 1;\n});\n\nit.each([[1]])('x %s', (n) => {\n  void n;\n});\n\ntest.each([[1]])('y %s', (n) => {\n  void n;\n});\n",
            &[(1, 1), (5, 1), (9, 1)],
        )
        .expect("the callback handed to what `.each(table)` returns is a test body");
}

#[test]
fn a_tagged_template_table_is_a_table_driven_test() {
    // The template-literal table makes the inner call's arguments a `template_string` rather
    // than an `arguments` node; both grammars are asserted, since tsx is a separate parse.
    for extension in ["ts", "tsx"] {
        tester_for(extension, "{}")
            .reports_at(
                "it.each`\n  a    | b\n  ${1} | ${2}\n`('$a and $b', ({ a, b }) => {\n  add(a, b)\n})\n",
                &[(1, 1)],
            )
            .expect("a template-literal table declares the same test");
    }
}

#[test]
fn a_typed_table_is_a_table_driven_test() {
    // TypeScript suites write the row type on the table call, which puts `type_arguments` on
    // the inner call; the callee the handler judges is still `test.each`.
    tester_for("ts", "{}")
        .reports_at(
            "test.each<[number, number]>([[1, 2]])('%i and %i', (a, b) => {\n  add(a, b)\n})\n",
            &[(1, 1)],
        )
        .expect("type arguments on the table call do not change the test shape");
}

#[test]
fn each_combined_with_a_modifier_is_a_test() {
    // Modifiers sit before `.each`, and jest documents them chained: `test.concurrent.only.each`.
    tester_for("ts", "{}")
        .reports_at(
            "it.only.each([1])('a %s', (n) => {\n  a(n)\n})\ntest.concurrent.each([1])('b %s', async (n) => {\n  b(n)\n})\nit.skip.each([1])('c %s', (n) => {\n  c(n)\n})\ntest.concurrent.only.each([1])('d %s', async (n) => {\n  d(n)\n})\n",
            &[(1, 1), (4, 1), (7, 1), (10, 1)],
        )
        .expect("a modifier before `.each` still declares a test");
}

#[test]
fn table_driven_groups_and_asserting_tables_are_fine() {
    // The over-widening direction: a describe table is a grouping, an asserting table passes,
    // and `each` called directly is not a test — `.each` is not a modifier.
    tester_for("ts", "{}")
        .accepts(
            "describe.each([1])('group %s', (n) => {\n  setup(n)\n})\ntest.describe.each([1])('pw %s', (n) => {\n  setup(n)\n})\nit.each([1])('ok %s', (n) => {\n  expect(n).toBe(1)\n})\nit.each('direct', () => {\n  run()\n})\n",
        )
        .expect("only a test callee's table declares a test, and asserting bodies pass");
}

#[test]
fn the_test_callees_option_names_more_tests() {
    // #292: a fixture-extended `test` under another name. A configured name is a base exactly
    // as `it` and `test` are, so the modifier and table forms come with it — and so does the
    // allow-list, which keeps its hooks and groups silent.
    tester_for("ts", "{ testCallees: ['pw'] }")
        .reports_at(
            "pw('a', async () => {\n  a()\n})\npw.only('b', () => {\n  b()\n})\npw.each([[1]])('c %s', (n) => {\n  c(n)\n})\npw.beforeEach(() => {\n  seed()\n})\npw.describe('group', () => {\n  setup()\n})\n",
            &[(1, 1), (4, 1), (7, 1)],
        )
        .expect("a configured callee declares tests in every form `test` does");
}

/// #292's repro: an aliased framework import, an aliased fixture import, and a plain `it`.
const ALIAS_REPRO: &str = "import { it as base } from 'vitest';\nimport { test as pw } from './fixtures';\n\nbase('aliased vitest', () => {\n  void 1;\n});\n\npw('fixture test', async () => {\n  void 2;\n});\n\nit('plain', () => {\n  void 3;\n});\n";

#[test]
fn an_aliased_framework_import_is_a_test() {
    // The framework alias is followed through its binding with no configuration; the fixture
    // module is not a framework, so its alias needs naming — and once named, all three report.
    tester_for("ts", "{}")
        .reports_at(ALIAS_REPRO, &[(4, 1), (12, 1)])
        .expect("`base` is vitest's `it`, so it declares a test");
    tester_for("ts", "{ testCallees: ['pw'] }")
        .reports_at(ALIAS_REPRO, &[(4, 1), (8, 1), (12, 1)])
        .expect("with `pw` named, every test in the repro is checked");
}

#[test]
fn an_alias_takes_modifiers_and_tables() {
    // An alias is a base like any other: its modifier and table forms are tests, its hooks and
    // groups are not. Both grammars, since tsx is a separate parse.
    for extension in ["ts", "tsx"] {
        tester_for(extension, "{}")
            .reports_at(
                "import { test as pwt } from '@playwright/test';\nimport { it as check } from '@jest/globals';\n\npwt.only('a', async () => {\n  a()\n});\npwt.describe('group', () => {\n  setup()\n});\npwt.beforeEach(() => {\n  seed()\n});\ncheck.each([[1]])('b %s', (n) => {\n  b(n)\n});\n",
                &[(4, 1), (13, 1)],
            )
            .expect("an aliased test callee keeps every form and the allow-list");
    }
}

#[test]
fn an_alias_of_a_framework_export_that_is_not_a_test_is_not_a_test() {
    // Following resolves the *export*, not the module: vitest's `describe` under another name
    // is still a group.
    tester_for("ts", "{}")
        .accepts(
            "import { describe as group, beforeEach as before } from 'vitest';\n\ngroup('g', () => {\n  setup()\n});\nbefore(() => {\n  seed()\n});\n",
        )
        .expect("only `it` and `test` exports declare a test");
}

#[test]
fn a_local_binding_shadowing_an_alias_is_not_a_test() {
    // Binding-exact: inside `run`, `base` is the parameter, not the import.
    tester_for("ts", "{}")
        .accepts(
            "import { it as base } from 'vitest';\n\nfunction run(base) {\n  base('not a test', () => {\n    work()\n  });\n}\n",
        )
        .expect("a shadowing local is not vitest's `it`");
}

#[test]
fn test_callees_must_be_plain_names() {
    // A string would make the membership test a substring test, and a dotted entry can never
    // equal a callee's base: both would be an option silently ignored, so both refuse to load.
    for options in [
        "{ testCallees: 'pw' }",
        "{ testCallees: ['test.describe'] }",
    ] {
        let error = tester_for("ts", options)
            .accepts("pw('a', () => {\n  expect(1).toBe(1)\n})\n")
            .expect_err("a malformed testCallees does not build");
        assert!(
            error.to_string().contains("testCallees"),
            "the refusal names the option for {options}: {error}"
        );
    }
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
