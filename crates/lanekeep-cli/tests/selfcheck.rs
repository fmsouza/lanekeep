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
fn configured(name: &str, source: &str, options: &str) -> RuleTester {
    RuleTester::configured(name, source, options).expect("the rule builds")
}

/// A factory rule whose subject files use a given extension.
///
/// `configured` writes the subject as `.ts` — the rule's own `language` is not consulted —
/// so a rule targeting Rust needs this to get a `.rs` subject the rule will actually run on.
fn configured_rs(name: &str, source: &str, options: &str) -> RuleTester {
    RuleTester::configured_with_extension(name, source, "rs", options).expect("the rule builds")
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

const ONE_PARSER: &str = include_str!("../../../lanekeep/rules/one-parser-per-file.ts");

fn one_parser() -> RuleTester {
    configured_rs("one-parser", ONE_PARSER, "{ allow: [] }")
}

#[test]
fn a_second_parser_is_reported() {
    one_parser()
        .reports_at(
            "fn go() {\n    let mut parser = tree_sitter::Parser::new();\n}\n",
            &[(2, 22)],
        )
        .expect("a parser outside the shared parse means the file is parsed twice");
}

#[test]
fn a_parser_in_a_test_module_passes() {
    // Panicking is the failure mechanism in a test, and so is parsing a fixture. The
    // exemption is what the deleted substring test achieved by splitting on `#[cfg(test)]`.
    one_parser()
        .accepts(
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        \
             let mut parser = tree_sitter::Parser::new();\n    }\n}\n",
        )
        .expect("test code parses its own fixtures");
}

#[test]
fn an_allowed_path_passes() {
    RuleTester::configured_with_extension(
        "one-parser-allow",
        ONE_PARSER,
        "rs",
        "{ allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let mut parser = tree_sitter::Parser::new();\n}\n")
    .expect("the two real parsers are named in lanekeep.json");
}

const CONTAINMENT: &str = include_str!("../../../lanekeep/rules/sandbox-containment.ts");

fn containment() -> RuleTester {
    configured_rs("containment", CONTAINMENT, "{ allow: [] }")
}

#[test]
fn naming_the_engine_is_reported() {
    containment()
        .reports_at("use rquickjs::Ctx;\n", &[(1, 1)])
        .expect("§5.1 keeps every line that knows QuickJS exists inside lanekeep-js");
}

#[test]
fn a_qualified_use_of_the_engine_is_reported_once() {
    // The query alternates over the use declaration and the path inside it, which match the
    // same line. One site is one violation.
    containment()
        .reports_at(
            "fn go() {\n    let c = rquickjs::Ctx::new();\n}\n",
            &[(2, 13)],
        )
        .expect("a qualified reference is the same violation as an import");
}

#[test]
fn the_sandbox_crate_passes() {
    RuleTester::configured_with_extension(
        "containment-allow",
        CONTAINMENT,
        "rs",
        "{ allow: ['subject/'] }",
    )
    .expect("the rule builds")
    .accepts("use rquickjs::Ctx;\n")
    .expect("lanekeep-js is where the engine is allowed to be named");
}

#[test]
fn an_unrelated_import_passes() {
    containment()
        .accepts("use std::collections::BTreeMap;\n")
        .expect("only the engine is contained");
}

const AUTHORITY: &str = include_str!("../../../lanekeep/rules/no-ambient-authority.ts");

fn authority() -> RuleTester {
    configured_rs("authority", AUTHORITY, "{ allow: [] }")
}

#[test]
fn a_subprocess_import_is_reported() {
    authority()
        .reports_at("use std::process::Command;\n", &[(1, 1)])
        .expect("nothing but changed.rs spawns a process");
}

#[test]
fn a_socket_is_reported() {
    authority()
        .reports_at("use std::net::TcpStream;\n", &[(1, 1)])
        .expect("§13's no network, ever, is enforced rather than aspirational");
}

#[test]
fn the_git_caller_passes() {
    RuleTester::configured_with_extension(
        "authority-allow",
        AUTHORITY,
        "rs",
        "{ allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("use std::process::Command;\n")
    .expect("--since shells out to git, and that is the one place");
}

#[test]
fn ordinary_std_passes() {
    authority()
        .accepts("use std::collections::BTreeMap;\nuse std::fs;\n")
        .expect("only network and subprocess are the boundary");
}

#[test]
fn exit_code_is_not_reported_but_command_still_is() {
    // `std::process` alone would also match `std::process::ExitCode`, which is how `main()`
    // reports its own exit status and reaches nothing outside the process. `process::Command`
    // is the capability the rule is actually about, and it still catches the qualified path.
    // Both imports in one subject so a `FORBIDDEN` list containing bare `std::process` — which
    // would report both lines — is what this test would catch.
    authority()
        .reports_at(
            "use std::process::Command;\nuse std::process::ExitCode;\n",
            &[(1, 1)],
        )
        .expect("ExitCode is not subprocess capability; Command still is");
}

const TRACKED: &str = include_str!("../../../lanekeep/rules/tracked-reads-only.ts");

fn tracked() -> RuleTester {
    configured_rs("tracked", TRACKED, "{ scope: ['subject/'], allow: [] }")
}

#[test]
fn an_untracked_read_is_reported() {
    tracked()
        .reports_at(
            "fn go() {\n    let t = std::fs::read_to_string(\"x\");\n}\n",
            &[(2, 13)],
        )
        .expect("a read that records no dependency makes the cache entry unsound");
}

#[test]
fn the_tracking_module_passes() {
    RuleTester::configured_with_extension(
        "tracked-allow",
        TRACKED,
        "rs",
        "{ scope: ['subject/'], allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let t = std::fs::read_to_string(\"x\");\n}\n")
    .expect("files.rs is the tracked-read implementation");
}

#[test]
fn a_file_outside_the_scope_passes() {
    RuleTester::configured_with_extension(
        "tracked-scope",
        TRACKED,
        "rs",
        "{ scope: ['crates/lanekeep-js/'], allow: [] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let t = std::fs::read_to_string(\"x\");\n}\n")
    .expect("the rule is about the sandbox crate, not the whole workspace");
}

#[test]
fn an_empty_scope_is_refused() {
    // `RuleTester::configured` only writes the fixture to disk; the factory is not called
    // until `run` loads the config, which is what `accepts` triggers below. The error surfaces
    // there, as a `TestError::Load`, not from `configured` itself.
    let error = RuleTester::configured_with_extension(
        "tracked-noscope",
        TRACKED,
        "rs",
        "{ scope: [], allow: [] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {}\n")
    .expect_err("a rule scoped to nothing must refuse to load rather than check nothing");
    // Assert on wording unique to the thrown message, not on a short word. `RuleTester`'s
    // temp directory embeds the tester's name and every `ConfigError`'s `Display` interpolates
    // that path, so `contains("scope")` would be satisfied by the path alone for any load
    // error at all — including one that has nothing to do with this guard.
    assert!(
        format!("{error}").contains("silently checks nothing"),
        "{error}"
    );
}

#[test]
fn a_use_declaration_reports_once() {
    // The site captured from `use_declaration argument: (_)` nests a `scoped_identifier` for
    // every `::` inside it. Nothing in the other cases exercises `isNestedInPath` filtering a
    // match down from a `use_declaration` ancestor specifically, rather than from another
    // `scoped_identifier` ancestor — this does.
    tracked()
        .reports_at("use std::fs::File;\n", &[(1, 1)])
        .expect("one violation for the declaration, not one per path segment");
}

#[test]
fn a_near_miss_identifier_is_not_reported() {
    // `vfs::` contains the substring `fs::`, which is enough to pass the `fileContains` gate,
    // but it is not the `fs` module — the regex requires `fs` as a whole path segment.
    tracked()
        .accepts("fn go() {\n    let t = vfs::something(\"x\");\n}\n")
        .expect("`vfs` is not `fs`; the gate's substring match is coarser than the check's");
}

const OBSERVATION: &str = include_str!("../../../lanekeep/rules/no-ambient-observation.ts");

fn observation() -> RuleTester {
    configured_rs(
        "observation",
        OBSERVATION,
        "{ scope: ['subject/'], allow: [] }",
    )
}

#[test]
fn a_clock_read_is_reported() {
    observation()
        .reports_at(
            "fn go() {\n    let n = std::time::SystemTime::now();\n}\n",
            &[(2, 13)],
        )
        .expect("a cached result computed from the clock is not reproducible");
}

#[test]
fn an_environment_read_is_reported() {
    observation()
        .reports_at(
            "fn go() {\n    let v = std::env::var(\"HOME\");\n}\n",
            &[(2, 13)],
        )
        .expect("the environment is not in the cache key");
}

#[test]
fn the_one_clock_site_passes() {
    RuleTester::configured_with_extension(
        "observation-allow",
        OBSERVATION,
        "rs",
        "{ scope: ['subject/'], allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let n = std::time::SystemTime::now();\n}\n")
    .expect("suppression::today is the one place lanekeep looks at the clock");
}

#[test]
fn an_empty_observation_scope_is_refused() {
    // Named distinctly from `tracked-reads-only`'s `an_empty_scope_is_refused` above: this
    // file is one module, and two `#[test]` functions sharing a name is `E0428`, not a
    // rule-behavior question — confirmed by compiling this file with the brief's literal
    // name before this rename.
    //
    // `RuleTester::configured` only writes the fixture to disk; the factory is not called
    // until `run` loads the config, which is what `accepts` triggers below. The error surfaces
    // there, as a `TestError::Load`, not from `configured` itself. Task 5 established this
    // shape the hard way — a test asserting on `configured`'s own Result passes whether the
    // guard exists or not.
    let error = RuleTester::configured_with_extension(
        "observation-noscope",
        OBSERVATION,
        "rs",
        "{ scope: [], allow: [] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {}\n")
    .expect_err("a rule scoped to nothing must refuse to load rather than check nothing");
    // Assert on wording unique to the thrown message, not on a short word. `RuleTester`'s
    // temp directory embeds the tester's name and every `ConfigError`'s `Display` interpolates
    // that path, so `contains("scope")` would be satisfied by the path alone for any load
    // error at all — including one that has nothing to do with this guard.
    assert!(
        format!("{error}").contains("silently checks nothing"),
        "{error}"
    );
}

const ITERATION: &str = include_str!("../../../lanekeep/rules/deterministic-iteration.ts");

fn iteration() -> RuleTester {
    configured_rs("iteration", ITERATION, "{ scope: ['subject/'] }")
}

#[test]
fn an_unordered_import_is_reported() {
    iteration()
        .reports_at("use std::collections::HashMap;\n", &[(1, 1)])
        .expect("iteration order here becomes report order or cache bytes");
}

#[test]
fn an_unordered_annotation_is_reported() {
    // Three distinct sites on one line would be three violations; this has one.
    iteration()
        .reports_at("fn go(m: HashSet<u8>) {}\n", &[(1, 10)])
        .expect("the type is the decision, wherever it is written");
}

#[test]
fn an_ordered_collection_passes() {
    iteration()
        .accepts("use std::collections::BTreeMap;\nfn go(m: BTreeSet<u8>) {}\n")
        .expect("BTreeMap and BTreeSet iterate in key order");
}

#[test]
fn a_file_outside_the_iteration_scope_passes() {
    // Named distinctly from `tracked-reads-only`'s `a_file_outside_the_scope_passes` above:
    // this file is one module, and two `#[test]` functions sharing a name is `E0428`, not a
    // rule-behavior question — the same collision the brief's empty-scope test already
    // anticipated for a different pair of names, just not for this one.
    RuleTester::configured_with_extension(
        "iteration-scope",
        ITERATION,
        "rs",
        "{ scope: ['crates/lanekeep-report/'] }",
    )
    .expect("the rule builds")
    .accepts("use std::collections::HashMap;\n")
    .expect("a HashMap whose order never leaves its crate is fine");
}

#[test]
fn an_empty_iteration_scope_is_refused() {
    // `RuleTester::configured` only writes the fixture to disk; the factory is not called
    // until `run` loads the config, which is what `accepts` triggers below. The error surfaces
    // there, as a `TestError::Load`, not from `configured` itself. Task 5 established this
    // shape the hard way — a test asserting on `configured`'s own Result passes whether the
    // guard exists or not.
    let error = RuleTester::configured_with_extension(
        "iteration-noscope",
        ITERATION,
        "rs",
        "{ scope: [] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {}\n")
    .expect_err("a rule scoped to nothing must refuse to load rather than check nothing");
    // Assert on wording unique to the thrown message, not on a short word. `RuleTester`'s
    // temp directory embeds the tester's name and every `ConfigError`'s `Display` interpolates
    // that path, so `contains("scope")` would be satisfied by the path alone for any load
    // error at all — including one that has nothing to do with this guard.
    assert!(
        format!("{error}").contains("silently checks nothing"),
        "{error}"
    );
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

const ENCODING: &str = include_str!("../../../lanekeep/rules/py-explicit-encoding.ts");

fn encoding() -> RuleTester {
    plain("encoding", ENCODING, "py")
}

#[test]
fn an_open_without_encoding_is_reported() {
    encoding()
        .reports_at("def go(p):\n    return open(p)\n", &[(2, 12)])
        .expect("Windows defaults to cp1252 and the failure is a truncated read");
}

#[test]
fn a_read_text_without_encoding_is_reported() {
    encoding()
        .reports_at("def go(p):\n    return Path(p).read_text()\n", &[(2, 12)])
        .expect("read_text takes the same default");
}

#[test]
fn a_write_text_without_encoding_is_reported() {
    // `NEEDS_ENCODING` lists three names, and the brief's own fixtures exercise only two of
    // them — `open` through the query's bare-identifier alternative, `read_text` through its
    // attribute alternative. A list entry no fixture ever reaches is indistinguishable from a
    // typo in that entry, so this closes the gap: `write_text` takes the same attribute-callee
    // shape as `read_text`, and this is the fixture that proves the pairing is real rather than
    // assumed.
    encoding()
        .reports_at(
            "def go(p, data):\n    return Path(p).write_text(data)\n",
            &[(2, 12)],
        )
        .expect("write_text takes the same default as read_text, and needs the same encoding");
}

#[test]
fn an_explicit_encoding_passes() {
    encoding()
        .accepts("def go(p):\n    a = open(p, encoding=\"utf-8\")\n    b = Path(p).read_text(encoding=\"utf-8\")\n")
        .expect("naming the encoding is the whole fix");
}

#[test]
fn a_call_to_len_passes() {
    // Named distinctly from `rule-declares-language.ts`'s `an_unrelated_call_passes` above:
    // this file is one module, and two `#[test]` functions sharing a name is `E0428`, not a
    // rule-behavior question — the same collision several rules above already ran into under
    // different names.
    encoding()
        .accepts("def go(p):\n    return len(p)\n")
        .expect("only the text-reading calls take an encoding");
}

#[test]
fn an_open_in_binary_mode_passes() {
    // `scripts/check_glibc_floor.py:67` — `open(path, "rb").read()` — reads raw ELF bytes.
    // `encoding=` is not merely unnecessary there, it is invalid: CPython raises `ValueError:
    // binary mode doesn't take an encoding argument` if it is passed alongside a binary mode.
    // Reporting this call would send an author to a change that breaks their script, which is
    // worse than not reporting it at all.
    encoding()
        .accepts("def go(p):\n    return open(p, \"rb\")\n")
        .expect("a binary open takes no encoding at all; there is nothing to add");
}

#[test]
fn an_open_with_mode_as_a_keyword_passes() {
    // The same binary-mode exemption, spelled with `mode=` rather than as the second
    // positional argument — proves `isBinaryMode` checks the keyword form and not only position.
    encoding()
        .accepts("def go(p):\n    return open(p, mode=\"rb\")\n")
        .expect("mode is still binary whether it is positional or a keyword");
}

#[test]
fn an_open_in_text_mode_is_still_reported() {
    // The test that matters most: without it, a mutation that made `isBinaryMode` always
    // return `true` would silence this rule for every `open` call, and every other test in
    // this file — none of which exercises plain text mode — would still pass.
    encoding()
        .reports_at("def go(p):\n    return open(p, \"r\")\n", &[(2, 12)])
        .expect("text mode still needs an explicit encoding; only binary is exempt");
}

#[test]
fn a_path_containing_b_is_still_reported() {
    // `isBinaryMode` counts positional arguments and only inspects the second one — the mode
    // — so a single-argument `open` whose *path* happens to contain the letter `b` must not be
    // mistaken for a binary mode string. This fixture has exactly one positional argument, the
    // path itself, and no mode at all: default text mode, still needs an encoding.
    encoding()
        .reports_at("def go():\n    return open(\"b.txt\")\n", &[(2, 12)])
        .expect("a path containing `b` is not a mode; only the second positional argument is");
}

#[test]
fn an_open_with_a_non_literal_mode_keyword_is_reported() {
    // The silencer this review round exists to catch. `mode=readable_mode`'s *whole* keyword
    // text contains a `b`, from partway through the identifier `readable_mode` itself and not
    // from any mode string — and a check that reads that whole text without first confirming
    // the value is a string literal treats this as binary and silently exempts a call that may
    // well be `mode="r"` at runtime and genuinely needs an encoding. The positional branch
    // already requires `kind === 'string'` before reading content; the keyword branch must hold
    // itself to the same discipline.
    encoding()
        .reports_at(
            "def go(p, readable_mode):\n    return open(p, mode=readable_mode)\n",
            &[(2, 12)],
        )
        .expect("a mode that is not a string literal cannot be proven binary, so it must still be reported");
}

#[test]
fn a_literal_mode_keyword_still_passes_after_the_string_check() {
    // Deliberately the same fixture as `an_open_with_mode_as_a_keyword_passes` above: this is
    // the regression the review round asked to be re-proven at the site of the fix, once the
    // keyword branch gained a `kind === 'string'` guard — the same intentional-redundancy
    // precedent already used elsewhere in this file (`node-handle-truthiness.ts`'s
    // `a_file_containing_only_ctx_root_is_reported`, kept "redundant against every mutation
    // tried so far... and that redundancy is intentional rather than an oversight"). Without
    // it, only `an_open_with_mode_as_a_keyword_passes` — written before the string check
    // existed — stands as evidence the new code path still accepts the literal case.
    encoding()
        .accepts("def go(p):\n    return open(p, mode=\"rb\")\n")
        .expect(
            "a string-literal mode keyword is exactly what the new check is supposed to accept",
        );
}

#[test]
fn a_buffering_keyword_is_not_mistaken_for_mode() {
    // The `Minor` finding as originally suggested: `buffering=1` is a real Python `open`
    // keyword whose *whole text* contains a `b` — its own first letter. Kept as direct evidence
    // this specific idiom is handled — but mutation-tested below (dropping the name filter)
    // and found NOT to isolate the name filter on its own: `1` is an `integer`, so the value's
    // own `kind === 'string'` check already saves this fixture regardless of whether the name
    // filter is present. `an_errors_keyword_with_a_string_value_is_not_mistaken_for_mode` below
    // is what actually closes the gap the `Minor` finding described, by using a keyword whose
    // *value* is a string containing `b` — the one case where only the name filter, not the
    // type check, can save it.
    encoding()
        .reports_at("def go(p):\n    return open(p, buffering=1)\n", &[(2, 12)])
        .expect("buffering is not mode; only a keyword named mode can indicate binary");
}

#[test]
fn an_errors_keyword_with_a_string_value_is_not_mistaken_for_mode() {
    // The fixture that actually isolates the name filter (`ctx.text(arg).startsWith('mode')`)
    // from the value-type check (`ctx.kind(value) === 'string'`) added earlier in this same
    // review round. `errors` is a real Python `open` keyword, and `"backslashreplace"` is a
    // real, valid value for it — a string, so it clears the type check — and it starts with
    // `b`. If the name filter were ever dropped or widened, this keyword's value alone would
    // be enough to make `isBinaryMode` return `true` and silently exempt a call that carries no
    // encoding at all. Confirmed by mutation: dropping the name filter entirely leaves
    // `a_buffering_keyword_is_not_mistaken_for_mode` green (see above) but reports nothing for
    // this fixture — this is the one that fails.
    encoding()
        .reports_at(
            "def go(p):\n    return open(p, errors=\"backslashreplace\")\n",
            &[(2, 12)],
        )
        .expect(
            "errors is not mode, and its value being a string that contains `b` must not matter",
        );
}

const STDOUT: &str = include_str!("../../../lanekeep/rules/py-stdout-buffer.ts");

fn stdout() -> RuleTester {
    plain("stdout", STDOUT, "py")
}

#[test]
fn a_text_write_to_stdout_is_reported() {
    stdout()
        .reports_at("import sys\nsys.stdout.write(\"hi\")\n", &[(2, 1)])
        .expect("stdout re-encodes to cp1252 on Windows and truncates");
}

#[test]
fn a_buffer_write_passes() {
    stdout()
        .accepts("import sys\nsys.stdout.buffer.write(b\"hi\")\n")
        .expect("bytes are neither re-encoded nor newline-translated");
}

#[test]
fn an_unrelated_write_passes() {
    // The brief's own fixture, `"def go(f):\n    f.write(\"hi\")\n"`, contains no substring
    // `sys.stdout` anywhere — the rule's own gate (`fileContains: ['sys.stdout']`) would
    // exclude it before the query ever runs, the same vacuous shape flagged repeatedly
    // elsewhere in this file. The trailing comment clears the gate without changing what is
    // called, so this actually exercises `ctx.text(m.obj) !== 'sys.stdout'`. Confirmed by
    // mutation: without the comment, deleting that guard still leaves this fixture passing —
    // for the wrong reason, since the file never reaches `check` either way.
    stdout()
        .accepts("def go(f):\n    f.write(\"hi\")\n# sys.stdout\n")
        .expect("only sys.stdout has the encoding problem");
}

#[test]
fn a_flush_call_on_stdout_passes() {
    // Every fixture above calls `.write`, so none of them can tell whether `check` filters by
    // method name at all, or whether the object-text guard is silently doing all the work on
    // its own — one guard masking another going untested, the same shape several rules earlier
    // in this file already had to close. `sys.stdout.flush()` clears the gate and still matches
    // the query (`.flush` is an attribute-form call exactly like `.write` is); only
    // `ctx.text(m.method) !== 'write'` keeps it unreported. Confirmed by mutation: deleting that
    // guard makes this fixture reported, which nothing else here would catch.
    stdout()
        .accepts("import sys\nsys.stdout.flush()\n")
        .expect("only write carries the encoding and newline problem; flush takes no text");
}

#[test]
fn a_print_call_passes() {
    // A real gap, documented rather than left to be discovered: `print` writes through
    // `sys.stdout` under the hood and fails the identical way on Windows, but this rule's query
    // only matches an attribute-form call (`(call function: (attribute ...))`), and `print(...)`
    // is a bare identifier call — there is no match for `check` to run against at all. That is a
    // narrower, different limitation than the object-text guard above: it is not that `check`
    // declines to report, it is that this fixture produces no match to decide on, so this test
    // does not exercise `check` and cannot be read as guard coverage. Recorded here because both
    // of this repository's own scripts write exclusively through `print`, which is why running
    // this rule over them finds nothing despite the shared underlying risk. The trailing comment
    // clears the gate; `print("hi")` alone contains no substring `sys.stdout`.
    stdout()
        .accepts("print(\"hi\")\n# sys.stdout\n")
        .expect("print shares the encoding problem but this rule's query does not reach it");
}

const HOST_API: &str = include_str!("../../../lanekeep/rules/host-api-matches-types.ts");

#[test]
fn a_registration_missing_from_the_types_is_reported() {
    // The rule reads the types through `ctx.readFile`, which is confined to the project root
    // — so the tester's own directory is the root, and the types file is written beside the
    // subject. `reports_messages` rather than `reports_at`: every finding is reported at the
    // file root, so the position says nothing and the message is the assertion.
    let tester = RuleTester::configured_with_extension(
        "host-api",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export interface RuleContext {\n  text(node: Node): string\n}\n",
        )
        .expect("the types fixture is written");

    tester
        .reports_messages(
            "fn r() {\n    object.set(\"text\", 1)?;\n    object.set(\"newThing\", 2)?;\n}\n",
            &["host.rs registers `newThing`, which packages/lanekeep/index.d.ts does not declare — it works but nobody can find it"],
        )
        .expect("a host function nobody can find is as good as absent");
}

#[test]
fn a_type_with_no_registration_is_reported() {
    let tester = RuleTester::configured_with_extension(
        "host-api-invented",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export interface RuleContext {\n  text(node: Node): string\n  invented(node: Node): string\n}\n",
        )
        .expect("the types fixture is written");

    tester
        .reports_messages(
            "fn r() {\n    object.set(\"text\", 1)?;\n}\n",
            &["index.d.ts declares `invented`, which host.rs does not register — autocomplete for a method that throws"],
        )
        .expect("a declared method that throws at run time is worse than no types");
}

#[test]
fn a_matching_pair_passes() {
    let tester = RuleTester::configured_with_extension(
        "host-api-clean",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export interface RuleContext {\n  text(node: Node): string\n}\n",
        )
        .expect("the types fixture is written");

    tester
        .accepts("fn r() {\n    object.set(\"text\", 1)?;\n}\n")
        .expect("registered and declared is the state this rule protects");
}

#[test]
fn a_registered_name_starting_with_r_is_recognized() {
    // The historical bug this guards, empirically: an early version stripped Rust string
    // prefixes with `/^["r#]+/`, greedy across `"` and `r` together, which ate the leading `r`
    // of names like `root`, `report` and `readFile` and invented four mismatches against the
    // real repository. None of this file's other fixtures register a name starting with `r`,
    // so none of them would have caught it — this one registers and declares `root` and
    // requires the pair to reconcile cleanly rather than becoming `root` vs. `oot`.
    let tester = RuleTester::configured_with_extension(
        "host-api-r-initial",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export interface RuleContext {\n  root(node: Node): string\n}\n",
        )
        .expect("the types fixture is written");

    tester
        .accepts("fn r() {\n    object.set(\"root\", 1)?;\n}\n")
        .expect("`root` registered and declared should reconcile, not become `oot`");
}

#[test]
fn a_registration_inside_test_code_is_ignored() {
    // `host.rs`'s own `#[cfg(test)] mod tests { ... }` never calls `object.set("literal",
    // ...)` today, so nothing in the real repository exercises this exclusion — this fixture
    // stands in for what a future test-only registration inside `host.rs` would look like.
    let tester = RuleTester::configured_with_extension(
        "host-api-test-code",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", "export interface RuleContext {\n}\n")
        .expect("the types fixture is written");

    tester
        .accepts(
            "#[cfg(test)]\nmod tests {\n    fn t() {\n        object.set(\"fakeThing\", 1)?;\n    }\n}\n",
        )
        .expect("a registration inside test code is not real API and must not be reported");
}

#[test]
fn the_real_host_and_types_reconcile() {
    // The deleted host_types.rs used include_str! against the real host.rs — a moved or
    // renamed file failed to COMPILE. This rule's `hostPath` is a runtime string instead, and
    // `check` returns early on `ctx.filePath !== hostPath`, so without an anchor like this one a
    // rename would make the rule silently check nothing on every file while `just lanekeep`
    // stayed green. Mirrors what `the_rule_still_matches_the_real_binding_source` already does
    // for binding-kinds-are-typed — and, on its own, would not be enough: see the floor test
    // below for why a second assertion is needed alongside this one.
    const REAL_HOST: &str = include_str!("../../lanekeep-js/src/host.rs");
    const REAL_TYPES: &str = include_str!("../../../packages/lanekeep/index.d.ts");

    let tester = RuleTester::configured_with_extension(
        "host-api-real",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", REAL_TYPES)
        .expect("the types fixture is written");

    tester
        .accepts(REAL_HOST)
        .expect("the real host API and its real published types should agree");
}

#[test]
fn the_rule_still_matches_the_real_host_source() {
    // Reconciling cleanly, above, is not enough on its own — a rule that returned early for
    // every file would also reconcile cleanly, vacuously. An empty `RuleContext` here proves
    // `check` actually read and matched the real host.rs's `object.set(` calls rather than
    // silently no-op-ing. There are roughly 31 `object.set(` call sites in host.rs; this floor
    // leaves generous headroom below the real, deduplicated count so one or two future
    // registrations do not make the test flaky. Same shape as
    // `the_rule_still_matches_the_real_binding_source` below.
    const REAL_HOST: &str = include_str!("../../lanekeep-js/src/host.rs");

    let tester = RuleTester::configured_with_extension(
        "host-api-floor",
        HOST_API,
        "rs",
        "{ hostPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", "export interface RuleContext {\n}\n")
        .expect("the types fixture is written");

    let violations = tester.run(REAL_HOST).expect("the rule runs");
    assert!(
        violations.len() >= 15,
        "expected the real host source to yield at least 15 registrations, got {}",
        violations.len()
    );
}

const KINDS: &str = include_str!("../../../lanekeep/rules/binding-kinds-are-typed.ts");

#[test]
fn a_binding_kind_missing_from_the_union_is_reported() {
    let tester = RuleTester::configured_with_extension(
        "kinds",
        KINDS,
        "rs",
        "{ bindingPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", "export type BindingKind =\n  | 'const'\n")
        .expect("the types fixture is written");

    tester
        .reports_messages(
            "impl BindingKind {\n    fn as_str(&self) -> &str {\n        match self {\n            \
             Self::Const => \"const\",\n            Self::Trait => \"trait\",\n        }\n    }\n}\n",
            &["`trait` is a binding kind the resolvers can return, and BindingKind does not include it — an author's switch silently never matches it"],
        )
        .expect("a narrowed union is wrong in a way that never errors");
}

#[test]
fn a_complete_union_passes() {
    let tester = RuleTester::configured_with_extension(
        "kinds-clean",
        KINDS,
        "rs",
        "{ bindingPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export type BindingKind =\n  | 'const'\n  | 'trait'\n",
        )
        .expect("the types fixture is written");

    tester
        .accepts(
            "impl BindingKind {\n    fn as_str(&self) -> &str {\n        match self {\n            \
             Self::Const => \"const\",\n            Self::Trait => \"trait\",\n        }\n    }\n}\n",
        )
        .expect("every kind typed is the state this protects");
}

#[test]
fn a_match_outside_as_str_and_kind_str_is_not_reported() {
    // Fix round 1: the query scans the whole file, so a `Display` impl or an error-message
    // match elsewhere in `binding.rs` must not be mistaken for a source of binding kinds.
    // Scoping is enforced by checking which function encloses each match arm.
    let tester = RuleTester::configured_with_extension(
        "kinds-scoped",
        KINDS,
        "rs",
        "{ bindingPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture(
            "types.d.ts",
            "export type BindingKind =\n  | 'const'\n  | 'trait'\n",
        )
        .expect("the types fixture is written");

    tester
        .accepts(
            "impl BindingKind {\n    fn as_str(&self) -> &str {\n        match self {\n            \
             Self::Const => \"const\",\n            Self::Trait => \"trait\",\n        }\n    }\n}\n\
             fn describe(kind: &BindingKind) -> &'static str {\n    match kind {\n        \
             BindingKind::Const => \"a compile-time constant\",\n        \
             BindingKind::Trait => \"a trait definition\",\n    }\n}\n",
        )
        .expect("a match in a function that is not as_str or kind_str is not scanned for kinds");
}

#[test]
fn a_kind_str_only_arm_is_reported_too() {
    // The exact wrong scoping the review warned against: narrowing `KIND_FUNCTIONS` to `as_str`
    // alone would silently drop `import`, which only `kind_str` produces — `Binding::Import` is
    // a separate arm of the enum from `BindingKind`, so `as_str` never sees it. Mutation-verified:
    // dropping `'kind_str'` from `KIND_FUNCTIONS` leaves this fixture accepted (reporting
    // nothing), which is exactly the silent failure this test exists to catch — none of the
    // other fixtures in this file contain a `kind_str`-shaped match, so none of them would.
    let tester = RuleTester::configured_with_extension(
        "kinds-kind-str",
        KINDS,
        "rs",
        "{ bindingPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", "export type BindingKind =\n  | 'const'\n")
        .expect("the types fixture is written");

    tester
        .reports_messages(
            "impl BindingKind {\n    fn as_str(&self) -> &str {\n        match self {\n            \
             Self::Const => \"const\",\n        }\n    }\n}\nimpl Binding {\n    \
             fn kind_str(&self) -> &str {\n        match self {\n            \
             Self::Import { .. } => \"import\",\n            Self::Local(kind) => kind.as_str(),\n        \
             }\n    }\n}\n",
            &["`import` is a binding kind the resolvers can return, and BindingKind does not include it — an author's switch silently never matches it"],
        )
        .expect("kind_str's arm is in scope too, not only as_str's");
}

#[test]
fn the_rule_still_matches_the_real_binding_source() {
    // The deleted host_types.rs carried a `found >= 10` floor for exactly this reason: a query
    // that silently stopped matching would make every assertion vacuous and the check would go
    // green forever. The synthetic fixtures above cannot catch that — they would keep matching
    // a two-arm toy file while the real one drifted out of reach.
    const BINDING: &str = include_str!("../../lanekeep-lang/src/binding.rs");

    let tester = RuleTester::configured_with_extension(
        "kinds-floor",
        KINDS,
        "rs",
        "{ bindingPath: 'subject/input.rs', typesPath: 'types.d.ts' }",
    )
    .expect("the rule builds");

    tester
        .write_fixture("types.d.ts", "export type BindingKind =\n  | 'nothing'\n")
        .expect("the types fixture is written");

    let violations = tester.run(BINDING).expect("the rule runs");
    assert!(
        violations.len() >= 10,
        "expected the real binding source to yield at least 10 kinds, got {}",
        violations.len()
    );
}
