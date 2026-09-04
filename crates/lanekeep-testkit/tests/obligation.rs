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
