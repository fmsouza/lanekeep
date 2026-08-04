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
    configured("one-parser", ONE_PARSER, "{ allow: [] }")
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
    RuleTester::configured(
        "one-parser-allow",
        ONE_PARSER,
        "{ allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let mut parser = tree_sitter::Parser::new();\n}\n")
    .expect("the two real parsers are named in lanekeep.json");
}

const CONTAINMENT: &str = include_str!("../../../lanekeep/rules/sandbox-containment.ts");

fn containment() -> RuleTester {
    configured("containment", CONTAINMENT, "{ allow: [] }")
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
    RuleTester::configured("containment-allow", CONTAINMENT, "{ allow: ['subject/'] }")
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
    configured("authority", AUTHORITY, "{ allow: [] }")
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
    RuleTester::configured(
        "authority-allow",
        AUTHORITY,
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
    configured("tracked", TRACKED, "{ scope: ['subject/'], allow: [] }")
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
    RuleTester::configured(
        "tracked-allow",
        TRACKED,
        "{ scope: ['subject/'], allow: ['subject/input.rs'] }",
    )
    .expect("the rule builds")
    .accepts("fn go() {\n    let t = std::fs::read_to_string(\"x\");\n}\n")
    .expect("files.rs is the tracked-read implementation");
}

#[test]
fn a_file_outside_the_scope_passes() {
    RuleTester::configured(
        "tracked-scope",
        TRACKED,
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
    let error = RuleTester::configured("tracked-noscope", TRACKED, "{ scope: [], allow: [] }")
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
    configured(
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
    RuleTester::configured(
        "observation-allow",
        OBSERVATION,
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
    let error = RuleTester::configured(
        "observation-noscope",
        OBSERVATION,
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
    configured("iteration", ITERATION, "{ scope: ['subject/'] }")
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
    RuleTester::configured(
        "iteration-scope",
        ITERATION,
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
    let error = RuleTester::configured("iteration-noscope", ITERATION, "{ scope: [] }")
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
