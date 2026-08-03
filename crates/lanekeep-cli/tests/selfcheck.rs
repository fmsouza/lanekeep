//! The project's own rules, run through the real engine.
//!
//! These are the rules in `lanekeep/rules/`, which encode this repository's architectural
//! invariants. Each one gets a case proving it reports: a rule that matches nothing is
//! indistinguishable from a rule that is broken, and that failure has happened here before.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

/// A rule with no options, tested against a subject with the given extension.
fn plain(name: &str, source: &str, extension: &str) -> RuleTester {
    RuleTester::with_extension(name, source, extension).expect("the rule builds")
}

/// A factory rule, called with the given options literal.
///
/// `RuleTester::configured` writes `rule({options})` into the generated config, so the module
/// must default-export a factory. Scope options must name `subject/`, which is where the
/// tester writes the file under test — a scope naming a real crate path matches nothing.
#[expect(
    dead_code,
    reason = "unused until a later task adds the first configured-rule case; kept from the \
              first commit so every task after this one calls it by the same name"
)]
fn configured(name: &str, source: &str, options: &str) -> RuleTester {
    RuleTester::configured(name, source, options).expect("the rule builds")
}

#[test]
fn the_harness_runs() {
    // Replaced by real cases in later tasks. Present so this file compiles and the
    // dev-dependency is exercised from the first commit.
    let rule = "import { defineRule } from 'lanekeep'\n\
                export default defineRule({\n\
                  id: 'local/probe',\n\
                  language: ['typescript'],\n\
                  severity: 'error',\n\
                  card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
                  query: '(debugger_statement) @s',\n\
                  check(ctx, m) { ctx.report(m.s) },\n\
                })\n";
    plain("probe", rule, "ts")
        .reports_at("debugger;\n", &[(1, 1)])
        .expect("the tester reports where the rule says");
}
