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

const GATES: &str = include_str!("../../../lanekeep/rules/gates-are-and.ts");

fn gates() -> RuleTester {
    plain("gates", GATES, "ts")
}

#[test]
fn a_two_substring_gate_is_reported() {
    gates()
        .reports_at(
            "const r = { gates: { fileContains: ['unwrap', 'expect'] } }\n",
            &[(1, 22)],
        )
        .expect("both substrings must be present, which rejects nearly every file");
}

#[test]
fn a_one_substring_gate_passes() {
    gates()
        .accepts("const r = { gates: { fileContains: ['makeStyles'] } }\n")
        .expect("one substring is the shape the gate is for");
}

#[test]
fn the_negative_form_is_checked_too() {
    gates()
        .reports_at(
            "const r = { gates: { fileNotContains: ['a', 'b'] } }\n",
            &[(1, 22)],
        )
        .expect("fileNotContains has the same conjunction");
}

#[test]
fn an_unrelated_array_passes() {
    // The trailing comment is load-bearing: the rule's own gate is `Contains`, and a fixture
    // that never contains that substring is rejected before the query runs at all. Without
    // it, this would pass whether or not `check` filters by key — which is the one thing
    // this case exists to prove.
    gates()
        .accepts("const r = { pathMatches: ['a', 'b'] } // Contains\n")
        .expect("only the content gates are conjunctions");
}

const DECLARES: &str = include_str!("../../../lanekeep/rules/rule-declares-language.ts");

fn declares() -> RuleTester {
    plain("declares", DECLARES, "ts")
}

#[test]
fn a_rule_without_a_language_is_reported() {
    declares()
        .reports_at(
            "const r = defineRule({ id: 'local/x', query: '(x) @y' })\n",
            &[(1, 11)],
        )
        .expect("the default is typescript and tsx, so a Rust rule would run on nothing");
}

#[test]
fn a_rule_with_a_language_passes() {
    declares()
        .accepts("const r = defineRule({ id: 'local/x', language: 'rust', query: '(x) @y' })\n")
        .expect("naming the language is the whole requirement");
}

#[test]
fn a_rule_with_several_languages_passes() {
    declares()
        .accepts("const r = defineRule({ id: 'local/x', language: ['typescript', 'tsx'] })\n")
        .expect("the default written out is still written out");
}

#[test]
fn an_unrelated_call_passes() {
    declares()
        .accepts("const c = defineConfig({ rules: [] })\n")
        .expect("only defineRule declares a language");
}

#[test]
fn a_same_shaped_call_under_a_different_name_passes() {
    // `an_unrelated_call_passes` above is excluded by the gate before the query ever runs —
    // `defineConfig` does not contain the substring `defineRule` — so it cannot tell whether
    // `check` itself filters by callee name. This fixture satisfies the gate on a comment
    // alone, leaving a call with the identical shape the query matches: an identifier callee
    // with a single object argument. Only the identifier check inside `check` keeps this
    // unreported.
    declares()
        .accepts("foo({ a: 1 }) // defineRule\n")
        .expect("the query matches any identifier call with an object argument; only defineRule is this rule's concern");
}

const REDUCE: &str = include_str!("../../../lanekeep/rules/reduce-touches-no-tree.ts");

fn reduce() -> RuleTester {
    plain("reduce", REDUCE, "ts")
}

#[test]
fn a_tree_call_in_reduce_is_reported() {
    reduce()
        .reports_at(
            "const r = {\n  reduce(ctx) {\n    const t = ctx.text(n)\n  },\n}\n",
            &[(3, 15)],
        )
        .expect("reduce receives facts and the file list, never trees");
}

#[test]
fn a_fact_call_in_reduce_passes() {
    reduce()
        .accepts("const r = {\n  reduce(ctx) {\n    for (const f of ctx.facts('e')) {}\n  },\n}\n")
        .expect("facts are what reduce is given");
}

#[test]
fn a_tree_call_in_check_passes() {
    // The trailing comment is load-bearing: the rule's own gate is `fileContains: ['reduce']`,
    // and a fixture that never contains that substring is rejected before the query runs at
    // all. Without it, this would pass whether or not `check` filters by method name — which
    // is the one thing this case exists to prove. Confirmed by mutation: deleting the
    // `ctx.text(m.name) !== 'reduce'` guard left this test passing until the comment was
    // added, because the gate alone was already keeping the file out.
    reduce()
        .accepts(
            "const r = {\n  check(ctx, m) {\n    const t = ctx.text(m.d)\n  },\n}\n// reduce\n",
        )
        .expect("check has a tree; that is the difference");
}

#[test]
fn a_non_ctx_receiver_named_like_a_tree_method_passes() {
    // The rule's known limitation, asserted rather than left to be discovered — the same
    // reason `no-unwrap`'s `a_method_named_expect_on_a_mock_is_still_reported` exists. The
    // `startsWith('ctx.')` guard is what keeps this rule from flagging `e.line` and
    // `cycle.column` inside `no-circular-imports.ts`'s own `reduce`, where `e` and `cycle` are
    // plain fact objects, not the sandbox context. Its only coverage before this test was
    // incidental: it worked because those two production files happen to name their loop
    // variables that way. Renaming them would have silently removed the only regression check
    // this guard had.
    reduce()
        .accepts("const r = {\n  reduce(ctx) {\n    const e = { line: 1 }\n    const x = e.line\n  },\n}\n")
        .expect("a property named like a tree method, on something that is not ctx, is not a tree call");
}

const FACTS: &str = include_str!("../../../lanekeep/rules/facts-must-serialize.ts");

fn facts() -> RuleTester {
    plain("facts", FACTS, "ts")
}

#[test]
fn a_node_handle_in_a_fact_is_reported() {
    facts()
        .reports_at(
            "const r = { check(ctx, m) { ctx.emitFact({ kind: 'e', node: m.decl }) } }\n",
            &[(1, 55)],
        )
        .expect("a handle is an index into a tree that will not exist when reduce runs");
}

#[test]
fn a_serializable_fact_passes() {
    facts()
        .accepts("const r = { check(ctx, m) { ctx.emitFact({ kind: 'e', name: ctx.text(m.decl), line: ctx.line(m.decl) }) } }\n")
        .expect("text, line and column survive JSON");
}

#[test]
fn a_serializable_member_expression_passes() {
    // The rule's known limitation, asserted rather than left to be discovered — the same
    // reason `reduce-touches-no-tree.ts`'s `a_non_ctx_receiver_named_like_a_tree_method_passes`
    // exists. `options.kind` is a member expression exactly like `m.decl` is, and it serializes
    // just as well; `startsWith('m.')` is the (convention-based, not binding-based) guard that
    // keeps this rule from flagging it. Confirmed by mutation: deleting
    // `ctx.text(value).startsWith('m.')` makes this fixture reported, which is precisely the
    // over-report this test exists to catch if it comes back.
    facts()
        .accepts(
            "const r = { check(ctx, m) { ctx.emitFact({ kind: 'e', tier: options.kind }) } }\n",
        )
        .expect("options.kind is a member expression too, but it is not a handle");
}

#[test]
fn a_call_rooted_at_the_match_still_passes() {
    // Closes a coverage gap the `startsWith('m.')` guard introduced (found while
    // mutation-testing the review's fix, not asked for directly): every other `accepts`
    // fixture's non-handle value starts with `ctx.` or `options.`, so the `m.`-prefix guard
    // alone would filter it out even if the `member_expression`-kind check just above it were
    // deleted entirely. Confirmed by mutation: deleting that kind check left
    // `a_serializable_fact_passes` and `a_serializable_member_expression_passes` both passing
    // regardless, because neither fixture's value has text starting with `m.`. `m.toString()`
    // does — it is a call on the match object itself, so it is only accepted because it is a
    // call, not because the guard below it never got a chance to fire.
    facts()
        .accepts(
            "const r = { check(ctx, m) { ctx.emitFact({ kind: 'e', label: m.toString() }) } }\n",
        )
        .expect("a call rooted at m is still a call, not a bare handle");
}

#[test]
fn an_unrelated_report_call_passes() {
    // Named distinctly from `rule-declares-language.ts`'s `an_unrelated_call_passes` above:
    // this file is one module, and two `#[test]` functions sharing a name is `E0428`, not a
    // rule-behavior question — the same collision several rules above already ran into under
    // different names.
    //
    // Two things load-bearing here, both confirmed by mutation. The trailing comment: the
    // rule's own gate is `fileContains: ['emitFact']`, and a fixture that never contains that
    // substring is rejected before the query runs at all — without it, this passes whether or
    // not `check` filters by method name. And the object argument itself: it holds a bare
    // member expression (`m.decl`), the same shape the rule reports on inside `emitFact`, rather
    // than a string. Deleting `ctx.text(m.method) !== 'emitFact'` left the brief's original
    // fixture (`{ message: 'x' }`) passing regardless — partly the gate, and partly that a
    // string value gives the deleted guard nothing to expose either way.
    facts()
        .accepts(
            "const r = { check(ctx, m) { ctx.report(m.decl, { node: m.decl }) } }\n// emitFact\n",
        )
        .expect("only emitFact has to survive the cache");
}

const HANDLE: &str = include_str!("../../../lanekeep/rules/node-handle-truthiness.ts");

fn handle() -> RuleTester {
    plain("handle", HANDLE, "ts")
}

#[test]
fn negating_a_node_handle_is_reported() {
    handle()
        .reports_at(
            "function f(ctx, m) {\n  const p = ctx.parent(m.n)\n  if (!p) return\n}\n",
            &[(3, 7)],
        )
        .expect("the root's handle is 0, so !p discards every top-level item");
}

#[test]
fn comparing_against_undefined_passes() {
    handle()
        .accepts(
            "function f(ctx, m) {\n  const p = ctx.parent(m.n)\n  if (p === undefined) return\n}\n",
        )
        .expect("undefined is the absent value; zero is a node");
}

#[test]
fn negating_something_else_passes() {
    // No trailing gate-clearing comment needed: `ctx.text` already contains `ctx.`, the gate's
    // substring since fix round 2 broadened it from `['ctx.parent']`. This fixture used to
    // carry a `// ctx.parent` comment for the narrower gate; removed rather than left pointless
    // once it stopped being necessary, per this repository's own rule about stale comments
    // claiming a reason that no longer holds.
    handle()
        .accepts("function f(ctx, m) {\n  const t = ctx.text(m.n)\n  if (!t) return\n}\n")
        .expect("text is a string and falsy means empty");
}

#[test]
fn negating_an_unrelated_variable_in_the_same_scope_passes() {
    // Closes a coverage gap the name-equality filter's absence would leave open (found while
    // mutation-testing this rule, not asked for directly) — the same reason
    // `facts-must-serialize.ts`'s `a_call_rooted_at_the_match_still_passes` exists. Every other
    // fixture's one unary expression already negates the node-returning variable itself, or
    // there is no unary expression at all, so neither would catch a `check` that reported on
    // any `!`-prefixed identifier found in scope, regardless of which variable it names.
    // Confirmed by mutation: deleting `ctx.text(hit.id) !== name` makes this fixture reported,
    // which is precisely the over-report this test exists to catch if it comes back.
    handle()
        .accepts(
            "function f(ctx, m) {\n  const p = ctx.parent(m.n)\n  const q = 0\n  if (!q) return\n}\n",
        )
        .expect("`q` never held a node handle; `p`, the ctx.parent result, is untouched");
}

#[test]
fn a_different_unary_operator_on_the_node_handle_passes() {
    // Closes a second coverage gap alongside the one above (found the same way, while
    // mutation-testing this rule): every other fixture's one unary expression on the
    // node-returning variable already starts with `!`, so none of them would catch a `check`
    // that reported on any unary operator at all — `typeof`, `-`, `void`, `~` — rather than
    // specifically negation. Confirmed by mutation: deleting
    // `!ctx.text(hit.neg).startsWith('!')` makes this fixture reported, which is precisely the
    // over-report this test exists to catch if it comes back.
    handle()
        .accepts(
            "function f(ctx, m) {\n  const p = ctx.parent(m.n)\n  if (typeof p === 'number') return\n}\n",
        )
        .expect("`typeof p` asks about the handle's JS type, not its truthiness");
}

#[test]
fn a_shadowed_variable_in_a_nested_function_is_still_reported() {
    // The rule's known limitation, asserted rather than left to be discovered — the same
    // reason `facts-must-serialize.ts`'s `a_serializable_member_expression_passes` and
    // `reduce-touches-no-tree.ts`'s `a_non_ctx_receiver_named_like_a_tree_method_passes` exist.
    // `closestAncestor`'s scope is the nearest enclosing `statement_block`, which for a
    // top-level declaration is the whole function body — including any function nested inside
    // it. The name check is textual (`ctx.text(hit.id) !== name`), not binding-based, so it
    // cannot tell this inner `p`, an ordinary number, from the outer one, a node handle: they
    // share a name and nothing else. This is a genuine over-report on legitimate code, not
    // merely a hypothetical — confirmed by running exactly this fixture and reading back
    // where it reported, per the same requirement that derived every other position in this
    // file from test output rather than from arithmetic.
    handle()
        .reports_at(
            "function f(ctx, m) {\n  const p = ctx.parent(m.n)\n  function g() {\n    const p = 0\n    if (!p) return\n  }\n}\n",
            &[(5, 9)],
        )
        .expect("g's own `p` is a plain number, but the rule cannot distinguish it from f's node handle by name alone");
}

#[test]
fn negating_ctx_root_is_reported() {
    // The purest instance of the bug this rule exists to catch: `ctx.root` is *always* handle
    // `0`, so `!r` here isn't a bug that might happen depending on which node was passed in —
    // it is one that happens every time. `ctx.root` is a property, not a call
    // (`readonly root: Node`), so it needs the query's second alternative rather than the
    // `call_expression` one `ctx.parent` matches. No trailing gate-clearing comment needed:
    // `ctx.root` itself contains `ctx.`, the gate's substring since fix round 2 — the
    // `// ctx.parent` this fixture used to carry, for the narrower gate that round closed, is
    // removed rather than left pointless.
    handle()
        .reports_at(
            "function f(ctx, m) {\n  const r = ctx.root\n  if (!r) return\n}\n",
            &[(3, 7)],
        )
        .expect("ctx.root is handle 0 unconditionally, not merely a call result that might be");
}

#[test]
fn negating_an_unrelated_root_property_passes() {
    // What stops the query's second alternative from becoming an over-report: `config.root` is
    // a `member_expression` shaped exactly like `ctx.root`, and a check keyed on the property
    // name alone (`.root`) rather than the full receiver-qualified text would flag a config
    // root, a directory root, or a tree root — precisely the over-reporting
    // `facts-must-serialize.ts` and `reduce-touches-no-tree.ts` both had to add a receiver
    // guard to avoid. Confirmed by mutation: relaxing `ctx.text(m.property) !== 'ctx.root'` to
    // a suffix check (`!endsWith('.root')`) makes this fixture reported, which is precisely the
    // over-report this test exists to catch if it comes back. Unlike the two tests above, the
    // trailing comment here is still load-bearing — `config.root` contains no `ctx.` anywhere,
    // and neither does the rest of the fixture (`ctx` appears only as a bare parameter name,
    // never followed by `.`) — simplified from `// ctx.parent` to `// ctx.` since fix round 2
    // broadened the gate to that substring.
    handle()
        .accepts("function f(ctx, m) {\n  const r = config.root\n  if (!r) return\n}\n// ctx.\n")
        .expect("config.root is an ordinary property; only ctx.root is always handle 0");
}

#[test]
fn a_file_containing_only_ctx_root_is_reported() {
    // Direct evidence the broadened gate closes the hole the narrow one left open: this
    // fixture contains no substring `ctx.parent` anywhere, including in comments — exactly the
    // case `gates: { fileContains: ['ctx.parent'] }` used to reject before the query ever ran,
    // even though `ctx.root` is unconditionally handle `0` and this is the guaranteed-bug shape
    // the rule's own docstring opens by describing. Deliberately the same shape as
    // `negating_ctx_root_is_reported` above, now that that test's own stale gate-clearing
    // comment is gone — the two are redundant against every mutation tried so far (confirmed:
    // reverting the gate fails both, not just this one), and that redundancy is intentional
    // rather than an oversight. This test exists as freestanding, explicitly-named evidence
    // that the gate itself is what changed, so a reader does not have to notice that the other
    // test happens to double as that proof only because its comment was removed in this same
    // round. Confirmed by mutation: reverting the gate to `['ctx.parent']` makes this fixture
    // (and `negating_ctx_root_is_reported`) produce no violations at all, not because the
    // detection logic is wrong but because the file is rejected before it is ever parsed — the
    // gate trap from the other direction, a gate narrow enough to hide the exact case a rule
    // exists to catch.
    handle()
        .reports_at(
            "function f(ctx, m) {\n  const root = ctx.root\n  if (!root) return\n}\n",
            &[(3, 7)],
        )
        .expect(
            "no ctx.parent substring anywhere; only the broadened `ctx.` gate lets this run at all",
        );
}

const NO_EVAL: &str = include_str!("../../../lanekeep/rules/no-eval-in-rules.ts");

fn no_eval() -> RuleTester {
    plain("no-eval", NO_EVAL, "ts")
}

#[test]
fn eval_is_reported() {
    no_eval()
        .reports_at("function f(src) {\n  return eval(src)\n}\n", &[(2, 10)])
        .expect("the sandbox cannot remove eval, so a rule using it is unreviewable by reading");
}

#[test]
fn the_function_constructor_is_reported() {
    no_eval()
        .reports_at(
            "function f(src) {\n  return new Function(src)\n}\n",
            &[(2, 10)],
        )
        .expect("the Function constructor is eval under another name");
}

#[test]
fn an_ordinary_function_passes() {
    no_eval()
        .accepts("function f() {\n  return 1\n}\n")
        .expect("writing the logic out is the remediation");
}

#[test]
fn a_call_to_an_unrelated_function_passes() {
    // Closes a coverage gap the brief's own fixture leaves open (found while mutation-testing
    // this rule, not asked for directly): `an_ordinary_function_passes` above contains no call
    // or `new` expression at all, so the query never matches it — it cannot tell a `check` that
    // reports on every match, regardless of name, from the real one. Confirmed by mutation:
    // deleting the whole guard (`if (name !== 'eval' && name !== 'Function') return`) leaves
    // every other case in this file passing regardless, this one included only once it exists.
    no_eval()
        .accepts("function f(src) {\n  return foo(src)\n}\n")
        .expect("an ordinary call is not a reviewability hazard; only eval is");
}

#[test]
fn constructing_an_unrelated_class_passes() {
    // The `new_expression` counterpart to the test above, closing the same gap for the query's
    // second alternative. Confirmed by mutation the same way: deleting the guard entirely makes
    // this fixture reported too, which is precisely the over-report these two tests exist to
    // catch if it comes back.
    no_eval()
        .accepts("function f(src) {\n  return new Array(src)\n}\n")
        .expect("an ordinary constructor is not a reviewability hazard; only Function is");
}
