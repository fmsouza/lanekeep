//! `lanekeep/no-restricted-imports`, run through the real engine.
//!
//! The subject file the harness writes is at `subject/input.ts`, which is what the `from`
//! patterns here are written against.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester(options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-restricted-imports").expect("the rule ships");
    RuleTester::configured("no-restricted-imports", source, options)
        .expect("builds")
        .with_builtins(lanekeep_rules::source)
}

#[test]
fn an_unrestricted_import_passes() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .accepts("import { join } from 'node:path';\n")
        .expect("nothing restricts node:path");
}

#[test]
fn a_restricted_module_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("import merge from 'lodash';\n", &[(1, 1)])
        .expect("lodash is restricted");
}

#[test]
fn no_restrictions_reports_nothing() {
    // The default. A rule configured with nothing must be inert rather than maximally
    // strict — the opposite would make adding the rule to a config a breaking change.
    tester("{}")
        .accepts("import merge from 'lodash';\n")
        .expect("an unconfigured restriction list restricts nothing");
}

#[test]
fn a_prefix_glob_matches_submodules() {
    let tester = tester("{ restrictions: [{ module: 'lodash/*' }] }");
    tester
        .reports_at("import merge from 'lodash/merge';\n", &[(1, 1)])
        .expect("the glob covers submodules");
    tester
        .accepts("import merge from 'lodash';\n")
        .expect("`lodash/*` does not match the bare package");
}

#[test]
fn a_glob_spans_separators() {
    // `@scope/*` has to reach `@scope/pkg/deep/thing`, or every restriction on a scoped
    // package would need a second entry for its subpaths.
    tester("{ restrictions: [{ module: '@internal/*' }] }")
        .reports_at("import x from '@internal/db/client';\n", &[(1, 1)])
        .expect("the glob spans separators");
}

#[test]
fn a_from_list_limits_where_the_restriction_applies() {
    let restricted = tester("{ restrictions: [{ module: 'stripe', from: ['subject/*'] }] }");
    restricted
        .reports_at("import Stripe from 'stripe';\n", &[(1, 1)])
        .expect("the subject is under subject/");

    let elsewhere = tester("{ restrictions: [{ module: 'stripe', from: ['packages/ui/*'] }] }");
    elsewhere
        .accepts("import Stripe from 'stripe';\n")
        .expect("the subject is not under packages/ui/");
}

#[test]
fn a_negated_from_entry_carves_out_an_exemption() {
    // The shape that makes this rule worth having: "nothing may import Stripe *except*
    // the payments package". Expressed as an enumeration of every other directory it would
    // rot the first time someone adds one.
    let exempt = tester("{ restrictions: [{ module: 'stripe', from: ['!subject/*'] }] }");
    exempt
        .accepts("import Stripe from 'stripe';\n")
        .expect("the subject is inside the carve-out");

    let not_exempt =
        tester("{ restrictions: [{ module: 'stripe', from: ['!packages/payments/*'] }] }");
    not_exempt
        .reports_at("import Stripe from 'stripe';\n", &[(1, 1)])
        .expect("the subject is outside the carve-out, so the restriction applies");
}

#[test]
fn an_exemption_wins_over_an_inclusion() {
    // Both lists in one entry. The exemption has to win, or "everything under src, except
    // src/legacy" would be inexpressible.
    tester("{ restrictions: [{ module: 'stripe', from: ['subject/*', '!subject/input*'] }] }")
        .accepts("import Stripe from 'stripe';\n")
        .expect("the carve-out overrides the inclusion");
}

#[test]
fn every_restricted_import_in_a_file_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }, { module: 'moment' }] }")
        .reports_at(
            "import merge from 'lodash';\nimport moment from 'moment';\nimport { join } from 'node:path';\n",
            &[(1, 1), (2, 1)],
        )
        .expect("both restricted imports are reported, the permitted one is not");
}

#[test]
fn only_one_violation_is_reported_per_import() {
    // Two restrictions match the same statement. Reporting twice would double-count a
    // single line and make the violation total useless as a measure of work to do.
    tester("{ restrictions: [{ module: 'lodash' }, { module: 'lodash*' }] }")
        .reports_at("import merge from 'lodash';\n", &[(1, 1)])
        .expect("overlapping restrictions still report once");
}

#[test]
fn the_reason_is_carried_into_the_message() {
    // The whole value of the rule for an agent reading the output: not "this is banned"
    // but what to do instead.
    let violations = tester(
        "{ restrictions: [{ module: 'stripe', reason: 'route it through @app/payments' }] }",
    )
    .run("import Stripe from 'stripe';\n")
    .expect("runs");

    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert_eq!(
        violation.rule_id.to_string(),
        "lanekeep/no-restricted-imports"
    );
    assert!(
        violation.message.contains("stripe"),
        "message does not name the module: {}",
        violation.message
    );
    assert!(
        violation.message.contains("route it through @app/payments"),
        "message does not carry the reason: {}",
        violation.message
    );
}

#[test]
fn a_missing_reason_still_produces_a_usable_message() {
    let violations = tester("{ restrictions: [{ module: 'stripe' }] }")
        .run("import Stripe from 'stripe';\n")
        .expect("runs");

    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert!(
        violation.message.contains("stripe"),
        "message does not name the module: {}",
        violation.message
    );
    assert!(
        !violation.message.contains("undefined"),
        "an absent reason leaked into the message: {}",
        violation.message
    );
}

// Every spelling below is a dependency on the module as real as an `import` declaration, so a
// restriction that only read `import` could be bypassed by respelling the line (#291).

#[test]
fn a_star_re_export_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("export * from 'lodash';\n", &[(1, 1)])
        .expect("`export * from` depends on the module");
}

#[test]
fn a_named_re_export_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("export { merge } from 'lodash';\n", &[(1, 1)])
        .expect("`export { x } from` depends on the module");
}

#[test]
fn a_namespace_re_export_is_reported() {
    tester("{ restrictions: [{ module: 'lodash/*' }] }")
        .reports_at("export * as fp from 'lodash/fp';\n", &[(1, 1)])
        .expect("`export * as ns from` depends on the module");
}

#[test]
fn a_type_only_re_export_is_reported() {
    // A type-only *import* is already reported, so a type-only re-export must be too, or the
    // two spellings of one dependency would disagree.
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("export type { LoDashStatic } from 'lodash';\n", &[(1, 1)])
        .expect("`export type { T } from` depends on the module");
}

#[test]
fn an_import_equals_require_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("import merge = require('lodash');\n", &[(1, 1)])
        .expect("TypeScript's `import x = require()` depends on the module");
}

#[test]
fn exports_that_name_no_module_pass() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .accepts(
            "export const lodash = 1;\nexport { lodash as merge };\nexport * from './local';\n",
        )
        .expect("neither a local export nor a permitted re-export is restricted");
}

#[test]
fn a_dynamic_import_is_reported_at_the_call() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at(
            "export async function load() { return import('lodash'); }\n",
            &[(1, 39)],
        )
        .expect("`import('m')` depends on the module");
}

#[test]
fn a_dynamic_import_with_attributes_is_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at(
            "const m = import('lodash', { with: { type: 'json' } });\n",
            &[(1, 11)],
        )
        .expect("the attributes argument does not hide the specifier");
}

#[test]
fn a_substitution_free_template_specifier_is_reported() {
    // Without a substitution a template literal is as static as a string literal; ignoring it
    // would make backticks a bypass.
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at(
            "const m = import(`lodash`);\nconst n = require(`lodash`);\n",
            &[(1, 11), (2, 11)],
        )
        .expect("a template literal with no substitution is a static specifier");
}

#[test]
fn a_computed_specifier_is_not_reported() {
    tester("{ restrictions: [{ module: 'lodash*' }] }")
        .accepts(
            "const name = 'lodash';\nconst a = import(name);\nconst b = import(`lodash/${name}`);\nconst c = require(name + '/merge');\n",
        )
        .expect("a specifier that is not static cannot be matched");
}

#[test]
fn a_require_call_is_reported_at_the_call() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at("const merge = require('lodash');\n", &[(1, 15)])
        .expect("`require('m')` depends on the module");
}

#[test]
fn side_effect_and_type_only_imports_are_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at(
            "import 'lodash';\nimport type { LoDashStatic } from 'lodash';\n",
            &[(1, 1), (2, 1)],
        )
        .expect("both import declarations depend on the module");
}

#[test]
fn a_require_made_by_create_require_is_reported() {
    // The commonest locally bound `require` is a real module loader, which is why `require`
    // is recognized by name rather than by what binds it.
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .reports_at(
            "import { createRequire } from 'node:module';\nconst require = createRequire(import.meta.url);\nconst merge = require('lodash');\n",
            &[(3, 15)],
        )
        .expect("a `require` from `createRequire` loads the module");
}

#[test]
fn a_call_that_is_not_require_is_not_reported() {
    // The callee name is gated in the query, so an ordinary call taking a string never
    // crosses into the sandbox at all — and if that gate ever stopped applying, this is the
    // test that turns red.
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .accepts("const label = t('lodash');\nconst other = requireLodash('lodash');\n")
        .expect("only a call to `require` loads a module");
}

#[test]
fn a_require_with_more_than_one_argument_is_not_reported() {
    tester("{ restrictions: [{ module: 'lodash' }] }")
        .accepts("const x = require('lodash', 1);\n")
        .expect("`require` takes one argument; two is some other function");
}

#[test]
fn every_spelling_from_the_issue_is_reported() {
    // The reproduction from #291, its six files folded into one.
    tester("{ restrictions: [{ module: 'bad-lib' }, { module: 'bad-lib/*' }] }")
        .reports_at(
            "import { x } from 'bad-lib';\n\
             export * from 'bad-lib';\n\
             export { y } from 'bad-lib';\n\
             export async function load() { return import('bad-lib'); }\n\
             import bad = require('bad-lib');\n\
             export const z = require('bad-lib');\n\
             export * as ns from 'bad-lib/sub';\n",
            &[(1, 1), (2, 1), (3, 1), (4, 39), (5, 1), (6, 18), (7, 1)],
        )
        .expect("every spelling of a dependency on a restricted module is reported");
}

#[test]
fn the_spellings_are_reported_in_tsx_too() {
    let source = lanekeep_rules::source("no-restricted-imports").expect("the rule ships");
    RuleTester::configured_with_extension(
        "no-restricted-imports",
        source,
        "tsx",
        "{ restrictions: [{ module: 'lodash' }] }",
    )
    .expect("builds")
    .with_builtins(lanekeep_rules::source)
    .reports_at(
        "export * from 'lodash';\nconst a = require('lodash');\nconst b = import('lodash');\nexport const C = () => <div />;\n",
        &[(1, 1), (2, 11), (3, 11)],
    )
    .expect("the tsx grammar shares the spellings");
}
