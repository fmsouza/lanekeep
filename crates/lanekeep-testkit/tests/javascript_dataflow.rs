//! Both dataflow analyses over the JavaScript grammar.
//!
//! `lanekeep-lang-js` builds one CFG for TypeScript, TSX and JavaScript, and until these
//! two cases existed nothing had run it against the JavaScript grammar — `JavaScript`
//! exposed a `flow_analyzer` and no `obligation_analyzer`, `docs/architecture.md` said the
//! CFG was TypeScript-and-TSX only, and the analyzer's own comment said it served all three
//! languages. A `.js` file through each analysis is what makes the three agree.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it already \
              makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

const FLOW_RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/secret-in-string',\n\
      language: ['javascript'],\n\
      requires: ['dataflow'],\n\
      flow: {\n\
        sources: ['(call_expression function: (identifier) @fn (#eq? @fn \"getSecret\")) @source'],\n\
        sinks: ['(call_expression function: (identifier) @fn (#eq? @fn \"log\") \
                 arguments: (arguments (_) @sink))'],\n\
        sanitizers: ['(call_expression function: (identifier) @fn (#eq? @fn \"redact\")) @sanitizer'],\n\
      },\n\
      card: { message: 'leak', remediation: 'redact', examples: { bad: 'log(s)', good: 'log(redact(s))' } },\n\
      checkFlow(ctx, path) { ctx.report(path.sink, 'reaches a sink'); },\n\
    });\n";

const OBLIGATION_RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/zeroed',\n\
      language: ['javascript'],\n\
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

fn flow() -> RuleTester {
    RuleTester::with_extension("js-flow", FLOW_RULE, "js").expect("builds")
}

fn obligation() -> RuleTester {
    RuleTester::with_extension("js-obligation", OBLIGATION_RULE, "js").expect("builds")
}

#[test]
fn taint_reaches_a_sink_in_a_javascript_file() {
    flow()
        .reports_at(
            "function f() { const s = getSecret(); log(s); }\n",
            &[(1, 43)],
        )
        .expect("the alias carries the secret to the sink");
}

/// `.jsx` parses under the same grammar, and a JSX expression container is an ordinary
/// expression to the graph — held to by a flow that crosses one.
#[test]
fn taint_reaches_a_sink_inside_jsx() {
    RuleTester::with_extension("jsx-flow", FLOW_RULE, "jsx")
        .expect("builds")
        .reports_at(
            "function f() { const s = getSecret(); return <div>{log(s)}</div>; }\n",
            &[(1, 56)],
        )
        .expect("the flow crosses a JSX expression container");
}

#[test]
fn a_sanitizer_cuts_taint_in_a_javascript_file() {
    flow()
        .accepts("function f() { log(redact(getSecret())); }\n")
        .expect("the sanitizer wraps the source");
}

#[test]
fn an_early_return_skips_release_in_a_javascript_file() {
    obligation()
        .reports_messages(
            "function f(c) { const b = acq(); if (c) { return; } rel(b); }\n",
            &["missed on some path"],
        )
        .expect("one path leaves the acquire undischarged");
}

#[test]
fn a_finally_discharges_on_every_path_in_a_javascript_file() {
    obligation()
        .accepts("function f() { const b = acq(); try { use(b); } finally { rel(b); } }\n")
        .expect("`finally` is on every path out");
}
