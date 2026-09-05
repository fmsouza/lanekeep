//! `lanekeep/no-secret-in-string`, run through the real engine.
//!
//! This is the acceptance gate for the whole taint-analysis stack (spec §11 of
//! `docs/superpowers/specs/2026-09-05-taint-analysis-flow-checkflow-design.md`): the rule
//! surface, config pairing, `FlowAnalyzer` and the engine's flow phase are exercised together,
//! through `RuleTester`, exactly as a project's own `flow`/`checkFlow` rule would be. Every
//! fixture is wrapped in `function f() { ... }` because the analyzer builds a per-function
//! CFG — the same shape `crates/lanekeep-engine/src/lib.rs`'s `a_flow_rule_reports_at_its_sink`
//! test uses.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helper below is neither, so the grant it already \
              makes for unit tests has to be restated for it."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-secret-in-string").expect("the rule ships");
    RuleTester::new("no-secret-in-string", source).expect("builds")
}

/// #1 — direct: a secret flows straight into the sink with nothing between them.
#[test]
fn direct_source_into_sink_reports() {
    tester()
        .reports_at("function f() { log(getSecret()); }\n", &[(1, 20)])
        .expect("`getSecret()` is the sink's own argument");
}

/// #2 — one intermediate assignment.
#[test]
fn one_assignment_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(s); }\n",
            &[(1, 43)],
        )
        .expect("`s`'s only definition is the tainted call");
}

/// #3 — two intermediate assignments: taint survives a second hop.
#[test]
fn two_assignments_report() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); const t = s; log(t); }\n",
            &[(1, 56)],
        )
        .expect("`t` aliases `s`, which is tainted");
}

/// #4 — a sanitizer placed **before** the sink cuts the flow: silent.
#[test]
fn sanitizer_before_sink_is_silent() {
    tester()
        .accepts("function f() { const s = getSecret(); const c = redact(s); log(c); }\n")
        .expect("`c`'s only definition is `redact(s)`, which is clean");
}

/// #5 — the same sanitizer, applied **after** the sink: flow-sensitivity means it does not
/// retroactively clean the read that already happened.
#[test]
fn sanitizer_after_sink_still_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(s); const c = redact(s); }\n",
            &[(1, 43)],
        )
        .expect("`log(s)` reads `s` while its only reaching definition is still tainted");
}

/// #6 — `const b = a` is a local alias and taint propagates through it.
#[test]
fn alias_reports() {
    tester()
        .reports_at(
            "function f() { const a = getSecret(); const b = a; log(b); }\n",
            &[(1, 56)],
        )
        .expect("`b` aliases `a` directly (an identifier RHS), which is tainted");
}

/// #7 — aliasing **through a call** does not propagate. Documented v1 false *negative*:
/// `identity(a)` is an opaque call to the analyzer, not an alias hop, exactly as
/// `crates/lanekeep-lang-js/src/flow.rs`'s `aliasing_through_a_call_does_not_propagate`
/// pins at the analyzer layer. Pinned here too because it is the trade a user of this rule
/// needs to know about, not only the analyzer's own author.
#[test]
fn alias_through_a_call_is_silent() {
    tester()
        .accepts("function f() { const a = getSecret(); const b = identity(a); log(b); }\n")
        .expect("v1 does not follow taint through a call's own arguments — a known limit");
}

/// #8 — field-insensitivity. Writing `o.secret` taints the whole `o` binding, and reading a
/// *different* field, `o.public`, is still tainted — a documented over-approximation (the
/// sound-leaning direction for a may-analysis), not a bug. Mirrors the analyzer's own fixture
/// at `crates/lanekeep-lang-js/src/flow.rs` (the §11 case in its test module).
#[test]
fn field_insensitive_write_taints_every_read_of_the_object() {
    tester()
        .reports_at(
            "function f() { const o = {}; o.secret = getSecret(); log(o.public); }\n",
            &[(1, 58)],
        )
        .expect("v1 is field-insensitive: a tainted base taints every field read from it");
}

/// #9 — a sink guarded by `if (isTest)` still reports. Path-insensitivity, documented: the
/// analysis asks only whether some path from the definition reaches the read, never whether
/// that branch is actually taken.
#[test]
fn a_sink_guarded_by_a_branch_still_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); if (isTest) { log(s); } }\n",
            &[(1, 57)],
        )
        .expect("path-insensitive: the guard does not suppress a reachable report");
}

/// #10 — two distinct sources, one sink. Both branches define `s` from their own `getSecret()`
/// call, and path-insensitivity unions both into the read: two distinct `(source, sink)` pairs,
/// not one, so dedup does not collapse them (that would hide a real second source) — the
/// analyzer's own `two_sources_into_one_sink_dedup_deterministically` pins the same fixture at
/// `flows.len() == 2`. What "dedup" means here is what it does *not* do: it does not multiply
/// a single source reaching by two paths into two reports (see the analyzer's sibling test
/// `one_source_reaching_a_sink_two_ways_is_deduplicated`), and it does not vary between runs —
/// both of which this fixture and #12 below assert.
#[test]
fn two_sources_into_one_sink_report_deterministically() {
    tester()
        .reports_at(
            "function f(c) { let s; if (c) { s = getSecret(); } else { s = getSecret(); } \
             log(s); }\n",
            &[(1, 82), (1, 82)],
        )
        .expect("one report per distinct source, both landing at the one sink");
}

// #11 — `flow` without `checkFlow` is refused at config load. That is a config-loading
// concern, not a rule-behavior one, and it is already covered where the pairing is
// implemented and validated: `crates/lanekeep-config/src/lib.rs`'s
// `flow_without_check_flow_is_refused` (Task 2, spec §4.2). Not duplicated here.

/// #13 — augmented assignment (`msg += getSecret()`) is a weak update that taints `msg`: the
/// string-concatenation shape this rule exists to catch. tree-sitter parses it as
/// `augmented_assignment_expression`, distinct from the strong `=` path, and the analyzer models
/// it as tainted-iff-RHS without killing prior taint — pinned at the analyzer layer by
/// `crates/lanekeep-lang-js/src/flow.rs`'s `an_augmented_assignment_from_a_source_reports`.
#[test]
fn augmented_assignment_from_a_source_reports() {
    tester()
        .reports_at(
            "function f() { let msg = \"\"; msg += getSecret(); log(msg); }\n",
            &[(1, 54)],
        )
        .expect("`msg += getSecret()` taints `msg`, read at the sink");
}

/// #12 — determinism: two runs over the same input are byte-identical. The two-source fixture
/// from #10 is the one that would expose a nondeterministic worklist or an unstable dedup/sort,
/// since it is the only fixture here with more than one flow into the same sink.
#[test]
fn two_runs_over_the_two_source_fixture_are_byte_identical() {
    let src = "function f(c) { let s; if (c) { s = getSecret(); } else { s = getSecret(); } \
               log(s); }\n";
    let rule = tester();
    let first = rule.run(src).expect("first run");
    let second = rule.run(src).expect("second run");
    assert_eq!(
        first, second,
        "two runs over identical input must be byte-identical"
    );
    assert_eq!(first.len(), 2, "both distinct sources report, every run");
}
