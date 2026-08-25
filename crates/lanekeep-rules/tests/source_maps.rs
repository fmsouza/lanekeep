//! A rule that throws names a line in the author's TypeScript, not one in the bundle.
//!
//! The four TypeScript built-ins are compiled ahead of time: four files and a shared module are
//! flattened into one entry module, which `componentize-js` turns into a StarlingMonkey
//! component. The positions in a thrown error's stack are therefore in that flattened module's
//! space — `matches@entry.js:957:16` — and `entry.js` is not a file anybody can open.
//!
//! **This is diagnostics and not correctness, and the distinction is worth keeping straight.** A
//! violation's position never comes from JavaScript: `report: func(n: node, …)` takes a node
//! handle and the host reads the position out of the Rust arena, and the reduce phase's
//! `reduce-location` carries a position a rule captured from a node earlier. The one thing a
//! source map affects is where a *thrown* error is reported, which is what this file is about.
//!
//! # What the assertion has to be
//!
//! That the frame names a file that exists and a line inside it. "Some position was reported" is
//! the assertion the specification warns against, and it is exactly what an unmapped frame
//! already satisfies: `entry.js:957:16` is a position, it is simply a position in a file the
//! author never wrote. So every case here reads the checked-in `.ts` and holds the reported line
//! to its length.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lanekeep_js::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_testkit::RuleTester;
use lanekeep_wasm::bindings::types::{MatchEntry, StackFrame};
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::{
    ComponentLoader, PermittedImports, RuleSet, WasmEngine, WasmError, WasmRuntime,
};

/// The repository root, so a frame's repository-relative path can be opened.
///
/// A test's working directory is its package root — `crates/lanekeep-rules` — and a frame names
/// a path relative to the repository, because that is the only spelling that means the same
/// thing to a user reading a diagnostic and to a `git grep`. Joining the two is not the same as
/// normalizing the path under test: the string being asserted is still the frame's own, and it
/// is asserted before it is joined to anything.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root is reachable from this crate")
}

/// Run one built-in rule's `check` until it throws, and hand back what the host was told.
///
/// The slot-taking `WasmRuntime::check` rather than the instance-taking `call_check`, because
/// the slot path is the one `lanekeep-engine` runs: a helper that took the other one would be
/// asserting about an entry point no run reaches.
fn run_until_it_throws(rule: &str, options: &str) -> (String, Vec<StackFrame>) {
    // The rule's id, as a config writes it. `lanekeep_rules::component` is keyed by the bare
    // name, and taking the id here keeps every case below spelled the way a user would see it
    // in a diagnostic.
    let name = rule.strip_prefix("lanekeep/").unwrap_or(rule);
    let (bytes, index) =
        lanekeep_rules::component(name).unwrap_or_else(|| panic!("`{rule}` ships as a component"));

    let engine = WasmEngine::new().expect("the shipped wasmtime configuration builds an engine");
    // Generous, on the terms `tests/component_rules.rs` sets: compiling a 12.4 MiB component is
    // seconds of work, and what is under test here is a position rather than a budget.
    let limits = Limits::default()
        .with_rule_timeout(Duration::from_mins(2))
        .with_global_timeout(Duration::from_mins(10));
    let clock = RunClock::start(limits.global_timeout);

    let loaded = ComponentLoader::without_cache()
        .permitting(PermittedImports::declared_world())
        .load_mapped(
            &engine,
            rule,
            bytes,
            lanekeep_rules::component_source_map(name),
        )
        .unwrap_or_else(|e| panic!("`{rule}` does not load: {e}"));
    assert!(
        loaded.has_source_map(),
        "`{rule}` ships without a source map, so its failures can only report positions in the \
         bundle it was compiled into"
    );

    let mut set = RuleSet::new(&engine).expect("the world links");
    let slot = set
        .add(rule, &loaded, index, options)
        .unwrap_or_else(|e| panic!("`{rule}` does not resolve against the world: {e}"));

    let mut runtime = WasmRuntime::for_rules(engine, Arc::new(set), limits, clock);
    let context = check_context(&mut runtime);
    // One capture, bound to the root. Every one of these rules reads its capture's text before
    // it reaches the code that fails, so what the node *is* does not matter — only that there is
    // one, and that the name is the one the rule's query binds.
    let captures = vec![MatchEntry {
        name: "source".to_owned(),
        node: 0,
    }];

    let error = runtime
        .check(slot, &context, &captures)
        .expect_err("the configured rule fails on this match");
    match error {
        WasmError::RuleFailed { message, frames } => (message, frames),
        other => panic!("`{rule}` failed in a way that carries no frames: {other:?}"),
    }
}

/// A check context over one trivial file, so `check` has something to be given.
fn check_context(runtime: &mut WasmRuntime) -> lanekeep_wasm::Resource<CheckContext> {
    let source = "import x from 'lodash/merge'\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the grammar loads");
    let tree = parser.parse(source, None).expect("it parses");

    runtime
        .host_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            "src/a.ts",
            Arc::new(TypeScript),
        ))
        .expect("the resource table accepts a context")
}

/// The options that make `no-restricted-imports` throw.
///
/// A restriction with no `module`, so the rule reaches `matches(undefined, specifier)` and
/// `undefined.split` throws. Provoked through the options rather than written into the rule
/// because the four rule sources are frozen: a migration that needed one of them edited would
/// have been the wrong migration.
const THROWS: &str = r#"{"restrictions": [{}]}"#;

#[test]
fn a_thrown_error_names_a_line_in_the_checked_in_typescript() {
    let (_, frames) = run_until_it_throws("lanekeep/no-restricted-imports", THROWS);

    let frame = frames.first().expect("a frame survives");
    assert_eq!(
        frame.file, "crates/lanekeep-rules/rules/no-restricted-imports.ts",
        "the innermost frame is in the bundle rather than in the rule: {frames:?}"
    );

    let source = std::fs::read_to_string(repository_root().join(&frame.file))
        .expect("the source the frame names ships");
    assert!(
        frame.line > 0 && frame.line as usize <= source.lines().count(),
        "line {} is past the end of a {}-line file",
        frame.line,
        source.lines().count()
    );

    // And the line is the one that throws, which is the assertion "inside the file" cannot
    // make: a map off by a few lines, or right about a neighboring statement, satisfies the
    // bound above and still sends a reader somewhere unrelated to the failure.
    let line = source
        .lines()
        .nth(frame.line as usize - 1)
        .expect("the line the frame names");
    assert!(
        line.contains("pattern.split"),
        "line {} is `{line}`, and the failure is `undefined.split`",
        frame.line
    );
}

#[test]
fn every_frame_that_survives_names_a_file_that_is_there() {
    // One stack, two sources: the rule, and the runtime that calls it. The per-frame assertion
    // above could hold for a map that is only right about the file its first segment happens to
    // name, and this is the one that cannot.
    let (_, frames) = run_until_it_throws("lanekeep/no-restricted-imports", THROWS);
    assert!(!frames.is_empty(), "the failure carried no stack at all");

    for frame in &frames {
        let path = repository_root().join(&frame.file);
        assert!(
            path.is_file(),
            "`{}` is not a file in this repository: {frames:?}",
            frame.file
        );
        let lines = std::fs::read_to_string(&path)
            .expect("the source the frame names is readable")
            .lines()
            .count();
        assert!(
            frame.line > 0 && frame.line as usize <= lines,
            "`{}` has {lines} lines and the frame names {}",
            frame.file,
            frame.line
        );
    }

    // The two spellings that must not survive, named rather than left to the loop above. The
    // first is the bundle, which is what the map exists to replace. The second is
    // `componentize-js`'s own generated glue, whose path is a temporary directory on whichever
    // machine built the artifact — it is in the shipped component's bytes, it exists nowhere
    // else, and showing it to a user is worse than showing nothing.
    let rendered = format!("{frames:?}");
    assert!(!rendered.contains("\"entry.js\""), "{rendered}");
    assert!(!rendered.contains("initializer.js"), "{rendered}");
}

/// Every path in the committed map, as the map spells it.
///
/// Read as text rather than through the decoder, because the decoder is not reachable from here
/// — it is private to `lanekeep-wasm` — and because what is under test is the *shipped file*
/// rather than anything Rust does with it.
fn committed_sources() -> Vec<String> {
    let json = std::fs::read_to_string(
        repository_root().join("crates/lanekeep-rules/components/typescript-builtins.wasm.map"),
    )
    .expect("the sidecar map ships beside the component");
    let map: serde_json::Value = serde_json::from_str(&json).expect("the map is JSON");

    map["sources"]
        .as_array()
        .expect("the map has a sources array")
        .iter()
        .map(|source| {
            source
                .as_str()
                .expect("every source is a string")
                .to_owned()
        })
        .collect()
}

#[test]
fn the_committed_map_names_repository_relative_paths_on_every_platform() {
    // The map is written by a recipe a maintainer runs on their own machine, and nothing about
    // that machine may reach the shipped file. Two ways it could: an absolute path, which names
    // a directory nobody else has, and a Windows separator, which is not what a frame's reader,
    // a `git grep` or an editor expects. Neither is visible from a remapped frame on the machine
    // that produced the map, which is why this is asserted against the file rather than against
    // a run.
    let sources = committed_sources();
    assert!(!sources.is_empty(), "the map names no sources at all");

    for source in &sources {
        assert!(
            !source.contains('\\'),
            "`{source}` is spelled with a Windows separator"
        );
        assert!(
            !source.starts_with('/') && !source.contains(':'),
            "`{source}` is absolute, so it names a directory on the machine that built it"
        );
        assert!(
            repository_root().join(source).is_file(),
            "`{source}` is not a file in this repository"
        );
    }
}

#[test]
fn the_map_covers_every_rule_the_component_hosts() {
    // One component, one map, four rules — so a map that had lost a rule's source would leave
    // exactly that rule reporting in bundle coordinates while the other three were fine. The
    // per-frame tests above run one rule, and could not see it.
    let sources = committed_sources();
    for rule in [
        "no-circular-imports",
        "no-default-export",
        "no-restricted-imports",
        "no-unused-exports",
    ] {
        let expected = format!("crates/lanekeep-rules/rules/{rule}.ts");
        assert!(
            sources.contains(&expected),
            "the map does not cover `{rule}`: {sources:?}"
        );
    }
}

/// `RuleTester::with_component_maps` is the published way to hand a built-in component's source
/// map to a tester, and it had no callers and no tests.
///
/// The map is diagnostics-only — it decides where a *thrown* error is reported and nothing
/// about what a rule finds — so the assertion is about the thrown frame's file, read out of the
/// run failure's rendered form. Without the map the frame stays in the bundle's space
/// (`entry.js`), which is the "rules that work and diagnostics that name `entry.js`, with
/// nothing anywhere going red" shape its own doc warns a wrong answer produces.
#[test]
fn a_thrown_built_in_names_the_authors_file_through_rule_tester() {
    let tester = RuleTester::for_built_in_configured(
        "no-restricted-imports",
        "ts",
        lanekeep_rules::component,
        THROWS,
    )
    .expect("the no-restricted-imports built-in ships as a component")
    .with_component_maps(lanekeep_rules::component_source_map);

    let error = tester
        .run("import x from 'lodash/merge'\n")
        .expect_err("the rule throws on a restriction with no `module`");
    let rendered = error.to_string();

    assert!(
        rendered.contains("crates/lanekeep-rules/rules/no-restricted-imports.ts"),
        "the frame was not remapped into the author's source: {rendered}"
    );
    // The bundle's own file is the bare `entry.js` the flattened rule module compiles into —
    // distinct from `packages/lanekeep/runtime/entry.js`, which is a real file the runtime
    // wrapper lives in and a frame is entitled to name. The map exists to replace the former.
    assert!(
        !rendered.contains("@entry.js:"),
        "an unmapped bundle frame survived into the diagnostic: {rendered}"
    );
}
