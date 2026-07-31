//! `lanekeep/no-default-export`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-default-export").expect("the rule ships");
    RuleTester::new("no-default-export", source).expect("builds")
}

#[test]
fn a_file_with_only_named_exports_passes() {
    tester()
        .accepts("export function parse() {}\nexport const VERSION = 1;\n")
        .expect("named exports are the point");
}

#[test]
fn a_default_function_export_is_reported() {
    tester()
        .reports_at("export default function parse() {}\n", &[(1, 1)])
        .expect("the default export is at the start of line 1");
}

#[test]
fn a_default_expression_export_is_reported() {
    // `export default <expr>` parses differently from `export default function` — a rule
    // matching only the declaration form would silently miss this.
    tester()
        .reports_at(
            "const parse = () => {};\nexport default parse;\n",
            &[(2, 1)],
        )
        .expect("the expression form is still a default export");
}

#[test]
fn a_default_class_export_is_reported() {
    tester()
        .reports_at("export default class Parser {}\n", &[(1, 1)])
        .expect("the class form is still a default export");
}

#[test]
fn every_default_export_in_a_file_is_reported() {
    // Two in one file is not valid TypeScript, but tree-sitter parses it anyway and the
    // rule must not stop at the first — a handler that returned early would pass every
    // single-violation test and under-report on real input.
    tester()
        .reports_at(
            "export default function a() {}\nexport default function b() {}\n",
            &[(1, 1), (2, 1)],
        )
        .expect("both are reported");
}

#[test]
fn the_word_default_elsewhere_is_not_a_default_export() {
    // The gate matches the bytes `default`, so this file reaches the query. What stops it
    // is the query itself, which is the division of labor the design intends: the gate is
    // allowed to be loose, the query is not allowed to be wrong.
    tester()
        .accepts(
            "const config = { export: 'default' };\n\
             function pick(x) { switch (x) { default: return config; } }\n\
             export { pick };\n",
        )
        .expect("a `default:` case label is not an export");
}

#[test]
fn re_exporting_a_default_from_elsewhere_is_reported() {
    // `export { default } from './x'` re-publishes a default export under this module's
    // name. Allowing it would let a codebase keep default exports by laundering them
    // through a re-export, which is exactly what a rule matching only `export default`
    // would permit. Reported at the `default` token, not the statement — it is the token
    // that has to change.
    tester()
        .reports_at("export { default } from './parse';\n", &[(1, 10)])
        .expect("a laundered default export is still one");
}

#[test]
fn aliasing_a_name_to_default_is_reported() {
    // No `export default` anywhere in the bytes, and still a default export.
    tester()
        .reports_at(
            "function parse() {}\nexport { parse as default };\n",
            &[(2, 19)],
        )
        .expect("the alias is what gets exported");
}

#[test]
fn aliasing_a_default_to_a_name_is_allowed() {
    // The mirror image, and the reason the rule looks at the alias rather than the name:
    // this takes someone else's default in and republishes it under a real name. That is
    // the fix, not the violation.
    tester()
        .accepts("export { default as parse } from './parse';\n")
        .expect("de-defaulting an import is the remedy");
}

#[test]
fn ordinary_named_re_exports_are_allowed() {
    tester()
        .accepts("export { parse, format as pretty } from './parse';\n")
        .expect("named re-exports are not default exports");
}

#[test]
fn the_message_names_the_problem_and_the_fix() {
    let violations = tester()
        .run("export default function parse() {}\n")
        .expect("runs");
    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };

    assert_eq!(violation.rule_id.to_string(), "lanekeep/no-default-export");
    assert!(
        violation.message.contains("default export"),
        "message does not name the problem: {}",
        violation.message
    );
    assert!(
        violation.remediation.contains("named export"),
        "remediation does not name the fix: {}",
        violation.remediation
    );
}
