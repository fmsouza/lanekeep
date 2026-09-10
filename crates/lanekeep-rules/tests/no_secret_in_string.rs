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

/// #4b — the source wrapped *directly* by the sanitizer inside the sink's own argument, with no
/// intermediate variable: the rule's own flagship doc example
/// (`crates/lanekeep-rules/rules/no-secret-in-string.ts`'s `log(redact(getSecret()))`, called
/// "silent" there). Distinct from #4: that fixture routes the sanitized value through a
/// reassignment, which an *unrelated* mechanism (v1 does not propagate taint through a call's own
/// arguments into a reassignment) already silences regardless of whether `redact` is even
/// recognized as a sanitizer. This fixture exercises the sanitizer capture itself, through the
/// real query-capture pipeline (`crates/lanekeep-engine/src/lib.rs`'s `collect_captures`) rather
/// than `flow.rs`'s own unit-test helper `calls_named`, which hand-builds whole-call sanitizer
/// nodes and so never exercised whether a query naming `@sanitizer` on the callee identifier —
/// the shape this rule shipped with — actually cuts a flow. It did not, until the sanitizer
/// query was changed to capture the whole call (`(call_expression ...) @sanitizer`) instead.
#[test]
fn sanitizer_wrapping_the_source_directly_is_silent() {
    tester()
        .accepts("function f() { log(redact(getSecret())); }\n")
        .expect("`redact(getSecret())` sanitizes its argument before it reaches the sink");
}

/// #4c — the same direct-wrap shape, inside a template substitution rather than a call argument.
/// Mirrors the corpus shape #218 names as its motivating fix target
/// (``secretKey=${describeBytes(account.secretKey)}`` at `migrateLegacyAccount.ts:81`).
#[test]
fn sanitizer_wrapping_the_source_in_a_template_is_silent() {
    tester()
        .accepts("function f() { log(`x=${redact(getSecret())}`); }\n")
        .expect("the template substitution's only content is the sanitized call");
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

/// #8 — field sensitivity (#225). Writing `o.secret` taints that path and not its siblings, so
/// reading `o.public` is clean while reading `o.secret` reports. This replaces the v1
/// over-approximation `field_insensitive_write_taints_every_read_of_the_object` asserted, and
/// it is a promise change: `docs/architecture.md` §4's sensitivity table and
/// `docs/built-in-rules.md`'s fixture table move with it. Mirrors the analyzer's own
/// `a_field_write_does_not_taint_a_sibling_field_read`.
#[test]
fn a_field_write_does_not_taint_a_sibling_field_read() {
    tester()
        .accepts("function f() { const o = {}; o.secret = getSecret(); log(o.public); }\n")
        .expect("`o.public` and `o.secret` are incomparable access paths");
}

/// #8b — the control for #8, and the half that would make the silence above worthless on its
/// own: the written path itself still reports.
#[test]
fn a_field_write_reports_at_its_own_path() {
    tester()
        .reports_at(
            "function f() { const o = {}; o.secret = getSecret(); log(o.secret); }\n",
            &[(1, 58)],
        )
        .expect("`o.secret` is exactly what was written");
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

/// #14 — a shape-property read off a tainted base is clean (#225, C2). `length`,
/// `byteLength`, `byteOffset` and `size` describe a value rather than carrying it, so the
/// read is clean while the base stays tainted. This is the whole of what the #220 corpus
/// calibration measured — `${seed.length}` at `keystore/sign.ts:112` — and closing it is why
/// that calibration now reports zero. Pinned at the analyzer layer by
/// `crates/lanekeep-lang-js/src/flow.rs`'s `a_length_read_off_a_tainted_base_is_clean`.
#[test]
fn a_shape_property_read_is_clean() {
    tester()
        .accepts("function f() { const s = getSecret(); log(s.length); }\n")
        .expect("`.length` of a secret is not the secret");
}

/// #14b — the corpus's own declaration shape: `s` is tainted at its root by a ternary of
/// sources, and the sink still reads only its length.
#[test]
fn a_shape_property_read_off_a_ternary_source_is_clean() {
    tester()
        .accepts(
            "function f(c) { const s = c ? getSecret().subarray(0, 32) : getSecret(); \
             log(s.length); }\n",
        )
        .expect("a root taint read through `.length` is still clean");
}

/// #14c — CONTROL. A *named* field of a secret carries it, and must still report: the shape
/// table is four names, not "any property". If this went silent, C2 would have closed the
/// calibration finding by silencing the analysis.
#[test]
fn a_named_property_read_off_a_secret_still_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(s.mnemonic); }\n",
            &[(1, 43)],
        )
        .expect("`s.mnemonic` carries the secret");
}

/// #14d — `byteLength` is in the table and `buffer` deliberately is not: one is the shape,
/// the other is the bytes.
#[test]
fn byte_length_is_clean_and_buffer_is_not() {
    tester()
        .accepts("function f() { const s = getSecret(); log(s.byteLength); }\n")
        .expect("`byteLength` is a shape property");
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(s.buffer); }\n",
            &[(1, 43)],
        )
        .expect("`buffer` is the value, not its shape");
}

/// #12 — determinism: two runs over the same input are byte-identical. The two-source fixture
/// from #10 is the one that would expose a nondeterministic worklist or an unstable dedup/sort,
/// since it is the only fixture here with more than one flow into the same sink.
#[test]
fn two_runs_over_the_two_source_fixture_are_byte_identical() {
    // Two independent testers, so each `run` is a cold run rather than a cache hit against the
    // same tester's directory — `RuleTester` keeps one directory per instance and never
    // disables the cache, so calling `run` twice on one tester compares a result with a copy
    // of itself and proves nothing about the analysis running twice.
    let src = "function f(c) { let s; if (c) { s = getSecret(); } else { s = getSecret(); } \
               log(s); }\n";
    let first = tester().run(src).expect("first run");
    let second = tester().run(src).expect("second run");
    assert_eq!(
        first, second,
        "two runs over identical input must be byte-identical"
    );
    assert_eq!(first.len(), 2, "both distinct sources report, every run");
}

/// #15 — a nested write is read at its own path, and is silent at a sibling of its last
/// segment. Depth is what distinguishes C1 from "one level of field awareness".
#[test]
fn a_nested_field_write_reports_at_its_own_path() {
    tester()
        .reports_at(
            "function f() { const o = { a: {} }; o.a.b = getSecret(); log(o.a.b); }\n",
            &[(1, 62)],
        )
        .expect("`o.a.b` is exactly what was written");
}

/// #15b — the sibling of #15.
#[test]
fn a_nested_field_write_is_silent_at_a_sibling() {
    tester()
        .accepts("function f() { const o = { a: {} }; o.a.b = getSecret(); log(o.a.c); }\n")
        .expect("`[a, b]` and `[a, c]` diverge at the second segment");
}

/// #15c — a read *above* a nested write reports: `o.a` is a value carrying the secret at
/// `.b`. Dropping this closure would silence `log(o)` after any write into `o`.
#[test]
fn a_read_above_a_nested_write_reports() {
    tester()
        .reports_at(
            "function f() { const o = { a: {} }; o.a.b = getSecret(); log(o.a); }\n",
            &[(1, 62)],
        )
        .expect("a read above a write covers it");
}

/// #16 — index sensitivity is explicitly not bought: every subscript collapses to one
/// segment, so `a[0] = getSecret(); log(a[1])` still reports. `docs/architecture.md` §4 still
/// says "Index-sensitive: no", and this is what holds it there.
#[test]
fn an_index_write_still_taints_every_index_read() {
    tester()
        .reports_at(
            "function f() { const a = []; a[0] = getSecret(); log(a[1]); }\n",
            &[(1, 54)],
        )
        .expect("`a[0]` and `a[1]` are one abstract path");
}

/// #17 — the alias pair. A local alias asks the aliased binding the same path question, so
/// `p.secret` reports and `p.public` is silent.
#[test]
fn an_alias_reads_the_written_path() {
    tester()
        .reports_at(
            "function f() { const o = {}; o.secret = getSecret(); const p = o; \
             log(p.secret); }\n",
            &[(1, 71)],
        )
        .expect("`p.secret` is `o.secret`");
}

/// #17b — the silent half of #17.
#[test]
fn an_alias_is_silent_at_a_sibling_path() {
    tester()
        .accepts(
            "function f() { const o = {}; o.secret = getSecret(); const p = o; \
             log(p.public); }\n",
        )
        .expect("`p.public` is `o.public`");
}

/// #18 — the widening. Paths longer than three segments truncate to their outermost three, and
/// a truncated path is top for its subtree: it matches every extension of itself, which is
/// today's whole-object behavior restored locally at depth. The bound is a stated number with
/// a stated behavior when hit, not a tuning knob.
#[test]
fn a_write_past_the_widening_bound_taints_its_siblings_below_it() {
    tester()
        .reports_at(
            "function f() { const o = { a: { b: { c: {} } } }; o.a.b.c.d = getSecret(); \
             log(o.a.b.c.e); }\n",
            &[(1, 80)],
        )
        .expect("`o.a.b.c.d` and `o.a.b.c.e` both truncate to `[a, b, c]`");
}

/// #19 — a cyclic object graph terminates and answers the same way every run. The cycle is a
/// loop-carried self-referential write, which is the only shape whose recursion is not bounded
/// by the read path shrinking; `MAX_DEPTH` is the backstop and nothing counts visits, so the
/// answer cannot depend on how far the walk got.
#[test]
fn a_cyclic_object_graph_terminates_and_reports() {
    tester()
        .reports_at(
            "function f(c) { const o = {}; o.secret = getSecret(); while (c) { o.next = o; } \
             log(o.next); }\n",
            &[(1, 85)],
        )
        .expect("the cycle terminates and the root taint is covered by the read");
}

/// #20 — determinism over the widening fixture: two runs byte-identical. The widening is where
/// a visit-count-keyed bound would have made the answer depend on traversal order, so it is
/// the fixture worth asserting this on rather than a shallow one.
#[test]
fn two_runs_over_the_widening_fixture_are_byte_identical() {
    // Two independent testers, on the same grounds as the sibling above: a single tester's
    // second `run` is a cache hit, not a second analysis. A second write is added so the
    // fixture carries two flows into the sink — both `o.a.b.c.d` and `o.a.b.c.f` truncate to
    // `[a, b, c]`, which meets the read at `o.a.b.c.e` — since a one-flow result cannot tell a
    // stable sort from a reversed one, which is what this pin needs to be able to catch.
    let src = "function f() { const o = { a: { b: { c: {} } } }; o.a.b.c.d = getSecret(); \
               o.a.b.c.f = getSecret(); log(o.a.b.c.e); }\n";
    let first = tester().run(src).expect("first run");
    let second = tester().run(src).expect("second run");
    assert_eq!(
        first, second,
        "two runs over identical input must be byte-identical"
    );
    assert_eq!(first.len(), 2, "both widened writes report, every run");
}

/// #21 — a computed-key write is read by name: `Index` is an unknown key, so it meets every
/// field. The field-insensitive analysis reported this too; a known-segment `Index` silenced
/// it (F1, review of #225's C1).
#[test]
fn a_computed_key_write_is_read_by_name() {
    tester()
        .reports_at(
            "function f() { const o = {}; o[\"secret\"] = getSecret(); log(o.secret); }\n",
            &[(1, 61)],
        )
        .expect("a secret stored under a computed key is still the secret");
}

/// #21b — the reverse direction, so neither arm of the comparison can be dropped alone.
#[test]
fn a_named_write_is_read_through_a_subscript() {
    tester()
        .reports_at(
            "function f() { const o = {}; o.secret = getSecret(); log(o[\"secret\"]); }\n",
            &[(1, 58)],
        )
        .expect("a secret read through a subscript is still the secret");
}

// --- #246: taint carried by a binding through a wrapping expression or a literal, end to end.
// The `flow.rs` unit tests pin the analyzer directly; these prove the same shapes survive the
// whole stack — query capture, config pairing, the engine's flow phase — as a project rule sees
// it. Before #246 every one of these was a silent miss: the shape fell through `taint_of`'s `_`.

/// #22 — a tainted binding wrapped in an object literal at the sink: `log({ cause: secret })`,
/// the headline row of the ticket. The only difference from the already-reporting
/// `log({ cause: getSecret() })` was whether the source was inlined or bound first.
#[test]
fn a_tainted_binding_in_an_object_literal_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log({ cause: s }); }\n",
            &[(1, 43)],
        )
        .expect("a tainted field taints the object whole");
}

/// #23 — a shorthand property `{ secret }` is `{ secret: secret }`, resolved as a reference.
#[test]
fn a_shorthand_property_reports() {
    tester()
        .reports_at(
            "function f() { const secret = getSecret(); log({ secret }); }\n",
            &[(1, 48)],
        )
        .expect("a shorthand names the binding it carries");
}

/// #24 — a tainted binding as an array element: `log([secret])`.
#[test]
fn a_tainted_binding_in_an_array_literal_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log([s]); }\n",
            &[(1, 43)],
        )
        .expect("an array element taints the array");
}

/// #25 — a tainted ternary branch: `log(c ? x : secret)`. The union of the two branches.
#[test]
fn a_tainted_ternary_branch_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(cond ? \"\" : s); }\n",
            &[(1, 43)],
        )
        .expect("a tainted branch taints the ternary");
}

/// #26 — a transparent wrapper at the sink: `log(secret!)`. Parentheses, `as`, `satisfies` and
/// `await` pass through the same way; the analyzer's own tests cover each.
#[test]
fn a_non_null_assertion_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); log(s!); }\n",
            &[(1, 43)],
        )
        .expect("`s!` is `s`");
}

/// #26b — the ticket's `const b = a as string; log(b)`: a cast on a binding's initializer,
/// reached through def-use rather than at the sink.
#[test]
fn a_cast_on_a_binding_reports() {
    tester()
        .reports_at(
            "function f() { const a = getSecret(); const b = a as string; log(b); }\n",
            &[(1, 66)],
        )
        .expect("a cast does not change the value");
}

/// #27 — field precision survives the new object arm: reading a *different*, known field of the
/// literal is silent. The control for #22, so the object arm cannot be a whole-object wildcard.
#[test]
fn an_untainted_object_literal_field_is_silent() {
    tester()
        .accepts("function f() { const s = getSecret(); const o = { cause: s }; log(o.other); }\n")
        .expect("o.cause and o.other are incomparable paths");
}

/// #28 — a transparent wrapper as a member *base* is peeled: `(o as any).token`, the idiomatic
/// cast-then-access. Transparency is symmetric — a wrapper is its inner value read whole and as
/// a base alike.
#[test]
fn a_cast_member_base_reports() {
    tester()
        .reports_at(
            "function f() { const s = getSecret(); const o = { token: s }; log((o as any).token); }\n",
            &[(1, 67)],
        )
        .expect("`(o as T).token` is `o.token`");
}
