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
