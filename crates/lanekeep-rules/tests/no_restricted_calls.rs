//! `lanekeep/no-restricted-calls`, run through the real engine.
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
    let source = lanekeep_rules::source("no-restricted-calls").expect("the rule ships");
    RuleTester::configured("no-restricted-calls", source, options).expect("builds")
}

#[test]
fn an_unrestricted_call_passes() {
    // A candidate callee that matches no restriction must not be reported — the
    // check reads its text once, matches nothing, and bails.
    tester("{ restrictions: [{ call: 'console.*' }] }")
        .accepts("const answer = Math.max(1, 2);\n")
        .expect("nothing restricts Math.max");
}

#[test]
fn a_restricted_bare_identifier_call_is_reported() {
    tester("{ restrictions: [{ call: 'fetch' }] }")
        .reports_at("fetch('/api/orders');\n", &[(1, 1)])
        .expect("fetch is restricted");
}

#[test]
fn a_restricted_member_call_is_reported() {
    tester("{ restrictions: [{ call: 'console.*' }] }")
        .reports_at("console.log('x');\n", &[(1, 1)])
        .expect("the glob covers members");
}

#[test]
fn no_restrictions_reports_nothing() {
    // The default. A rule configured with nothing must be inert rather than maximally
    // strict — the opposite would make adding the rule to a config a breaking change.
    tester("{}")
        .accepts("console.log('x');\n")
        .expect("an unconfigured restriction list restricts nothing");
}

#[test]
fn a_glob_spans_member_separators() {
    // `api.*` has to reach `api.client.fetch`, or every restriction on a chain of
    // qualifiers would need an entry per depth.
    tester("{ restrictions: [{ call: 'api.*' }] }")
        .reports_at("api.client.fetch('/x');\n", &[(1, 1)])
        .expect("the glob spans the member separators");
}

#[test]
fn a_whitespace_split_member_still_matches() {
    // The callee text is normalized before matching: `console\n  .log` reads as
    // `console.log`, so a restriction written against what an author types also
    // matches what a formatter breaks across lines.
    tester("{ restrictions: [{ call: 'console.*' }] }")
        .reports_at("console\n  .log('x');\n", &[(1, 1)])
        .expect("whitespace is stripped before matching");
}

#[test]
fn an_optional_chained_call_still_matches() {
    // `?.` is folded to `.` before matching, so `console?.log` matches `console.*`.
    tester("{ restrictions: [{ call: 'console.*' }] }")
        .reports_at("console?.log('x');\n", &[(1, 1)])
        .expect("`?.` reads as `.`");
}

#[test]
fn a_from_list_limits_where_the_restriction_applies() {
    let restricted = tester("{ restrictions: [{ call: 'fetch', from: ['subject/*'] }] }");
    restricted
        .reports_at("fetch('/api/orders');\n", &[(1, 1)])
        .expect("the subject is under subject/");

    let elsewhere = tester("{ restrictions: [{ call: 'fetch', from: ['packages/ui/*'] }] }");
    elsewhere
        .accepts("fetch('/api/orders');\n")
        .expect("the subject is not under packages/ui/");
}

#[test]
fn a_negated_from_entry_carves_out_an_exemption() {
    // The shape that makes this rule worth having: "no `fetch` anywhere *except* the
    // API client". Expressed as an enumeration of every other directory it would rot
    // the first time someone adds one.
    let exempt = tester("{ restrictions: [{ call: 'fetch', from: ['!subject/*'] }] }");
    exempt
        .accepts("fetch('/api/orders');\n")
        .expect("the subject is inside the carve-out");

    let not_exempt = tester("{ restrictions: [{ call: 'fetch', from: ['!packages/api/*'] }] }");
    not_exempt
        .reports_at("fetch('/api/orders');\n", &[(1, 1)])
        .expect("the subject is outside the carve-out, so the restriction applies");
}

#[test]
fn an_exemption_wins_over_an_inclusion() {
    // Both lists in one entry. The exemption has to win, or "everything under src,
    // except src/legacy" would be inexpressible.
    tester("{ restrictions: [{ call: 'fetch', from: ['subject/*', '!subject/input*'] }] }")
        .accepts("fetch('/api/orders');\n")
        .expect("the carve-out overrides the inclusion");
}

#[test]
fn every_restricted_call_in_a_file_is_reported() {
    tester("{ restrictions: [{ call: 'console.*' }, { call: 'fetch' }] }")
        .reports_at(
            "console.log('a');\nfetch('/b');\nMath.max(1, 2);\n",
            &[(1, 1), (2, 1)],
        )
        .expect("both restricted calls are reported, the permitted one is not");
}

#[test]
fn only_one_violation_is_reported_per_call() {
    // Two restrictions match the same call. Reporting twice would double-count a
    // single line and make the violation total useless as a measure of work to do.
    tester("{ restrictions: [{ call: 'console.*' }, { call: 'console*' }] }")
        .reports_at("console.log('x');\n", &[(1, 1)])
        .expect("overlapping restrictions still report once");
}

#[test]
fn the_first_matching_restriction_wins_and_carries_its_reason() {
    // The first entry that matches decides the message: a reason on a later entry
    // must not leak in, or a restriction list could not give one reason per shape.
    let violations = tester(
        "{ restrictions: [{ call: 'console.*', reason: 'route it through the logger' }, \
          { call: 'console.*', reason: 'never' }] }",
    )
    .run("console.log('x');\n")
    .expect("runs");

    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert_eq!(violation.rule_id.to_string(), "lanekeep/no-restricted-calls");
    assert!(
        violation.message.contains("console.log"),
        "message does not name the callee: {}",
        violation.message
    );
    assert!(
        violation.message.contains("route it through the logger"),
        "message does not carry the first reason: {}",
        violation.message
    );
    assert!(
        !violation.message.contains("never"),
        "a later restriction's reason leaked into the message: {}",
        violation.message
    );
}

#[test]
fn a_missing_reason_still_produces_a_usable_message() {
    let violations = tester("{ restrictions: [{ call: 'console.*' }] }")
        .run("console.log('x');\n")
        .expect("runs");

    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert!(
        violation.message.contains("console.log"),
        "message does not name the callee: {}",
        violation.message
    );
    assert!(
        !violation.message.contains("undefined"),
        "an absent reason leaked into the message: {}",
        violation.message
    );
}
