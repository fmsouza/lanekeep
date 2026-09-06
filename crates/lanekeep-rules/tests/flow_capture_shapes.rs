//! Which node a `flow` role's capture binds decides whether the analyzer can use it at all,
//! and the difference is invisible from a query that compiles: `@sanitizer` on a callee
//! identifier loads, matches, and never cuts a flow (#222, #223). These cases drive a project
//! rule through `RuleTester` — config load, query compilation and the analyzer together, the
//! path a real rule takes — so the contract is pinned where an author would meet it rather
//! than only in `lanekeep-config`'s unit tests.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helper below is neither, so the grant it already \
              makes for unit tests has to be restated for it."
)]

use lanekeep_testkit::{RuleTester, TestError};

/// A flow rule whose sink is `log(...)`'s argument, with the given sanitizer and source queries.
fn rule(sources: &str, sanitizers: &str) -> String {
    format!(
        "import {{ defineRule }} from 'lanekeep';\n\
         export default defineRule({{\n\
           id: 'local/flow',\n\
           requires: ['dataflow'],\n\
           severity: 'error',\n\
           flow: {{\n\
             sources: [{sources}],\n\
             sinks: ['(call_expression function: (identifier) @fn (#eq? @fn \"log\") \
                      arguments: (arguments (_) @sink))'],\n\
             sanitizers: [{sanitizers}],\n\
           }},\n\
           card: {{ message: 'm', remediation: 'r', examples: {{ bad: 'a', good: 'b' }} }},\n\
           checkFlow(ctx, path) {{ ctx.report(path.sink, 'reaches'); }},\n\
         }});\n"
    )
}

const SOURCE_ON_THE_CALL: &str =
    r#"'(call_expression function: (identifier) @fn (#eq? @fn "getSecret")) @source'"#;
const SOURCE_ON_THE_CALLEE: &str =
    r#"'(call_expression function: (identifier) @source (#eq? @source "getSecret"))'"#;
const SANITIZER_ON_THE_CALL: &str =
    r#"'(call_expression function: (identifier) @fn (#eq? @fn "redact")) @sanitizer'"#;
const SANITIZER_ON_THE_CALLEE: &str =
    r#"'(call_expression function: (identifier) @sanitizer (#eq? @sanitizer "redact"))'"#;

fn tester(sources: &str, sanitizers: &str) -> RuleTester {
    RuleTester::new("flow-capture-shapes", &rule(sources, sanitizers)).expect("builds")
}

/// The documented shape: the whole call is the sanitizer, so its result is clean.
#[test]
fn a_sanitizer_capturing_the_whole_call_cuts_the_flow() {
    let tester = tester(SOURCE_ON_THE_CALL, SANITIZER_ON_THE_CALL);
    tester
        .accepts("function f() { log(redact(getSecret())); }\n")
        .expect("the sanitizer wraps the source");
    // The control: the same rule, the same file minus the sanitizer, does report — so the
    // silence above is the sanitizer's doing and not a rule that never ran.
    tester
        .reports_at("function f() { log(getSecret()); }\n", &[(1, 20)])
        .expect("the bare source reaches the sink");
}

/// The #223 shape is refused at load, before any file is read, naming the rule and the capture.
#[test]
fn a_sanitizer_captured_on_the_callee_is_refused_before_anything_runs() {
    let tester = tester(SOURCE_ON_THE_CALL, SANITIZER_ON_THE_CALLEE);
    match tester.run("function f() { log(redact(getSecret())); }\n") {
        Err(TestError::Load(message)) => {
            assert!(message.contains("local/flow"), "{message}");
            assert!(message.contains("@sanitizer"), "{message}");
            assert!(message.contains("callee"), "{message}");
        }
        other => panic!("expected a load refusal, got {other:?}"),
    }
}

/// The sink twin of the same footgun.
#[test]
fn a_sink_captured_on_the_callee_is_refused_before_anything_runs() {
    let source = "import { defineRule } from 'lanekeep';\n\
        export default defineRule({\n\
          id: 'local/flow', requires: ['dataflow'], severity: 'error',\n\
          flow: {\n\
            sources: ['(call_expression function: (identifier) @fn (#eq? @fn \"getSecret\")) @source'],\n\
            sinks: ['(call_expression function: (identifier) @sink (#eq? @sink \"log\"))'],\n\
          },\n\
          card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
          checkFlow(ctx, path) { ctx.report(path.sink, 'reaches'); },\n\
        });\n";
    let tester = RuleTester::new("flow-sink-on-callee", source).expect("builds");
    match tester.run("function f() { log(getSecret()); }\n") {
        Err(TestError::Load(message)) => {
            assert!(message.contains("local/flow"), "{message}");
            assert!(message.contains("@sink"), "{message}");
            assert!(message.contains("callee"), "{message}");
        }
        other => panic!("expected a load refusal, got {other:?}"),
    }
}

/// The refusal's boundary: a `@source` on the callee is a working shape — the callee sits
/// inside the call the sink reads — and must keep loading and seeding.
#[test]
fn a_source_captured_on_the_callee_still_seeds() {
    tester(SOURCE_ON_THE_CALLEE, SANITIZER_ON_THE_CALL)
        .reports_at(
            "function f() { const s = getSecret(); log(s); }\n",
            &[(1, 43)],
        )
        .expect("the callee-captured source seeds `s`");
}
