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
    RuleTester::configured("no-restricted-imports", source, options).expect("builds")
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
