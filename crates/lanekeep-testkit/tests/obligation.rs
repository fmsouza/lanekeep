//! End-to-end coverage for #193's typestate obligation dispatch.
//!
//! `RuleTester` runs the real `Engine`, so this is the first exercise of the whole pipeline:
//! `EXTRACT` carrying `obligation`/`checkObligation` off the rule module, `lanekeep-config`
//! loading it now that `dataflow` is implemented, `Engine::prepare` compiling the acquire and
//! release queries, and `Engine::run_rule`'s obligation arm calling into the analyzer and then
//! into `checkObligation` itself.
//!
//! Filed here rather than in `crates/lanekeep-engine/tests/`, where the design for this task
//! placed it: `lanekeep-testkit` depends on `lanekeep-engine`, so a dev-dependency the other
//! way would close a cycle in the publication order exactly as the comment beside
//! `lanekeep-engine`'s own `[dev-dependencies]` already describes for `lanekeep-rules` — see
//! that crate's `Cargo.toml`. `RuleTester`'s whole point is running the real engine end to
//! end, so the property under test is identical either way.
//!
//! `RULE` also pins Ruling 2 (the plan's `matches.is_empty()` guard) for free: it declares no
//! `query` and no `check`, so both tests below only ever reach a violation through the
//! obligation arm — there is no main-query match loop for either to fall back on.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. `tester()` below is neither, so the grant it already \
              makes for unit tests has to be restated for it."
)]

use lanekeep_testkit::RuleTester;

const RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/zeroed',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'zero it', remediation: 'rel(b) on all paths',\n\
              examples: { bad: 'const b = acq();', good: 'const b = acq(); rel(b);' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'missed on some path' : 'never released');\n\
      },\n\
    });\n";

fn tester() -> RuleTester {
    RuleTester::new("zeroed", RULE).expect("builds")
}

#[test]
fn reports_when_an_early_return_skips_release() {
    tester()
        .reports_messages(
            "function f(c) { const b = acq(); if (c) { return; } rel(b); }\n",
            &["missed on some path"],
        )
        .expect("early return leaves one path undischarged");
}

#[test]
fn silent_when_released_on_all_paths() {
    tester()
        .accepts("function f() { const b = acq(); rel(b); }\n")
        .expect("released on the only path");
}

/// `scope: 'block'`, run end to end. `crates/lanekeep-lang-js/src/obligation.rs` documents
/// the mechanism and carries the same two fixtures directly against `JsObligationAnalyzer`:
/// the analyzer resolves the acquire's nearest `statement_block` ancestor and keeps only
/// releases whose byte range sits inside it, so a release lexically after that block cannot
/// discharge an acquire inside it even though nothing here gives the two distinct control-flow
/// blocks. These two are the `RuleTester` equivalent, exercised through query matching,
/// `Engine::run_rule`'s obligation arm, and `checkObligation` itself.
const BLOCK: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/block-zero',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'block',\n\
      },\n\
      card: { message: 'not released before block exit', remediation: 'rel before leaving',\n\
              examples: { bad: '{ const b = acq(); }', good: '{ const b = acq(); rel(b); }' } },\n\
      checkObligation(ctx, u) { ctx.report(u.exit, 'left the block undischarged'); },\n\
    });\n";

fn block() -> RuleTester {
    RuleTester::new("block-zero", BLOCK).expect("builds")
}

#[test]
fn released_before_block_exit_is_silent() {
    block()
        .accepts("function f() { { const b = acq(); rel(b); } after(); }\n")
        .expect("released inside the block");
}

#[test]
fn released_after_the_block_reports() {
    block()
        .reports_messages(
            "function f() { { const b = acq(); } rel(b); }\n",
            &["left the block undischarged"],
        )
        .expect("the release is outside the block");
}

/// No top-level `query`, no `check` — only `obligation` and `checkObligation`. Pins two things
/// at once: that a rule built from only those two loads at all (the relaxations `build_rule`
/// grants an obligation-only rule), and that `Engine::run_rule`'s
/// `matches.is_empty() && obligation.is_none()` early return does not fire just because
/// `matches` is empty — with no main query, `matches` is *always* empty, so this can only ever
/// report through the obligation arm.
const ONLY: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/only',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'x', remediation: 'y', examples: { bad: 'acq()', good: 'acq(); rel()' } },\n\
      checkObligation(ctx, u) { ctx.report(u.exit, 'unmet'); },\n\
    });\n";

#[test]
fn an_obligation_only_rule_with_no_query_still_runs() {
    RuleTester::new("only", ONLY)
        .expect("builds")
        .reports_messages("function f() { const b = acq(); }\n", &["unmet"])
        .expect("no main query must not skip the obligation arm");
}

/// #193's real acceptance rule, `secrets-zeroed-on-all-paths`. Where `RULE` above uses bare
/// `acq`/`rel` calls to pin the wiring, this exercises a realistic acquire/release shape — a
/// member-call acquire (`e.getEntropy()`/`e.deriveSeed()`), a two-form release (`.fill(0)`
/// with a literal zero argument, or a `zeroBytes(...)` helper call) — against the seven
/// function-scope behaviors the feature's acceptance contract lists.
///
/// Namespaced `local/`, not the design doc's own illustrative `pera/`: `RuleTester`'s
/// generated `lanekeep.config.ts` declares no `namespaces`, and `rule_id::Namespace`'s two
/// built-ins — `lanekeep` and `local` — are exactly the ones that need no declaring. A
/// project's real `lanekeep.json` would declare `pera` and use it there; this fixture proves
/// the CFG/analyzer wiring, which the namespace does not touch.
const SECRETS: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/secrets-zeroed-on-all-paths',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (member_expression property: (property_identifier) @m) \
                    (#any-of? @m \"getEntropy\" \"deriveSeed\")) @acquire'],\n\
        release: ['(call_expression function: (member_expression property: (property_identifier) @p) \
                    (#eq? @p \"fill\") arguments: (arguments (number) @z) (#eq? @z \"0\")) @release',\n\
                  '(call_expression function: (identifier) @f (#eq? @f \"zeroBytes\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'secret buffer not zeroed on all paths',\n\
              remediation: 'call .fill(0) or zeroBytes on every path, e.g. in finally',\n\
              examples: { bad: 'const b = e.getEntropy();',\n\
                          good: 'const b = e.getEntropy(); try {} finally { b.fill(0); }' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'zeroed on some paths, not all' : 'never zeroed');\n\
      },\n\
    });\n";

fn secrets() -> RuleTester {
    RuleTester::new("secrets", SECRETS).expect("builds")
}

#[test]
fn happy_path_only_reports_partial() {
    secrets()
        .reports_messages(
            "function f(c) { const b = e.getEntropy(); if (c) { return; } b.fill(0); }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("early return skips the fill");
}

#[test]
fn zeroed_in_finally_is_silent() {
    secrets()
        .accepts(
            "function f() { const b = e.getEntropy(); try { use(b); } finally { b.fill(0); } }\n",
        )
        .expect("finally is on all paths");
}

#[test]
fn zeroed_only_after_throw_reports() {
    secrets()
        .reports_messages(
            "function f(c) { const b = e.getEntropy(); if (c) { throw x; } b.fill(0); }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("the throw path never zeroes");
}

#[test]
fn never_zeroed_reports_never() {
    secrets()
        .reports_messages(
            "function f() { const b = e.getEntropy(); }\n",
            &["never zeroed"],
        )
        .expect("no fill anywhere");
}

#[test]
fn zeroed_on_both_branches_is_silent() {
    secrets()
        .accepts("function f(c) { const b = e.getEntropy(); if (c) { b.fill(0); } else { b.fill(0); } }\n")
        .expect("both branches discharge");
}

#[test]
fn zeroed_in_a_maybe_zero_iteration_loop_reports() {
    secrets()
        .reports_messages(
            "function f(xs) { const b = e.getEntropy(); for (const x of xs) { b.fill(0); } }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("a zero-iteration loop skips the fill");
}

#[test]
fn zero_bytes_helper_discharges() {
    secrets()
        .accepts("function f() { const b = e.getEntropy(); zeroBytes(b); }\n")
        .expect("the helper release form counts");
}

/// The acquire's `#any-of? @m "getEntropy" "deriveSeed"` has two literals; every fixture above
/// exercises only the first. `deriveSeed` is otherwise identical — a member-call acquire with
/// no release — so this pins the second literal without adding a new behavior to reason about.
#[test]
fn derive_seed_unzeroed_reports_never_zeroed() {
    secrets()
        .reports_messages(
            "function f() { const b = e.deriveSeed(); }\n",
            &["never zeroed"],
        )
        .expect("deriveSeed is the acquire's second #any-of? literal");
}

/// `AGENTS.md`'s determinism invariant, applied to the obligation arm specifically: "two runs
/// over identical input produce byte-identical output." Nothing about the CFG build, the
/// analyzer's witness search, or `checkObligation` should depend on anything but `source`.
#[test]
fn two_runs_are_byte_identical() {
    let t = secrets();
    let src = "function f(c) { const b = e.getEntropy(); if (c) { return; } b.fill(0); }\n";
    let a = t.run(src).expect("run a");
    let b = t.run(src).expect("run b");
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}
