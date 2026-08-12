//! Rebuilding a Python rule can change its behavior without changing its source.
//!
//! `CPython`'s hash seed is drawn from `wasi:random` during pre-init and frozen into the
//! heap image — `hash('lanekeep')` differs per build and is stable per artifact. A rule
//! that iterates a `set` of strings gets an order set by that seed, so the same source
//! built twice can report different violations. Sorting violations by
//! `(ruleId, file, line, column)` does not rescue a rule that picks *which* node to
//! report by set order.
//!
//! There is no runtime reset available — the seed is baked into the heap image, and
//! `PYTHONHASHSEED` in the host environment cannot help because pre-init runs `CPython`
//! *inside* the guest. The mitigation is authoring guidance: sort (`sorted()`) when order
//! matters, and never pick "the first" of a set or stop after N. This file demonstrates
//! both halves against two builds of one source, so the guidance is grounded in evidence
//! rather than assertion.
//!
//! `just py-rules` builds the probe twice and sets `LANEKEEP_PY_DETERMINISM_A` and
//! `LANEKEEP_PY_DETERMINISM_B`; without them, this test skips.

#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types;
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::{ComponentLoader, RuleSet, WasmEngine, WasmRuntime};

/// One probe artifact, or None when the recipe has not built it (the test skips).
fn artifact(name: &str) -> Option<Vec<u8>> {
    let path = std::env::var(name).ok()?;
    std::fs::read(path).ok()
}

/// Drive one probe artifact's `check` and return its three observables.
fn observables(bytes: &[u8]) -> (String, String, String) {
    let engine = WasmEngine::new().expect("an engine");
    let loaded = ComponentLoader::without_cache()
        .load(&engine, "probe", bytes)
        .expect("imports are permitted");

    let mut set = RuleSet::new(&engine).expect("a set");
    let slot = set.add("probe", &loaded, 0, "null").expect("added");

    let limits = Limits::default();
    let clock = RunClock::start(limits.global_timeout);
    let mut runtime = WasmRuntime::for_rules(engine, Arc::new(set), limits, clock);

    let source = "x = 1\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the grammar loads");
    let tree = parser.parse(source, None).expect("it parses");
    let arena = NodeArena::new(tree, source.to_owned());
    let context = runtime
        .host_mut()
        .push_check_context(CheckContext::new(arena, "probe.py", Arc::new(TypeScript)))
        .expect("the resource table accepts a context");

    let captures = vec![types::MatchEntry {
        name: "id".to_owned(),
        node: NodeArena::ROOT,
    }];
    runtime
        .check(slot, &context, &captures)
        .expect("check runs without trapping");

    let reports = runtime
        .host_mut()
        .check_context_mut(&context)
        .expect("the context outlives the call")
        .take_reports();

    let mut hash = String::new();
    let mut set_order = String::new();
    let mut sorted = String::new();
    for report in &reports {
        let message = report.message.as_deref().unwrap_or("");
        if let Some(v) = message.strip_prefix("hash=") {
            hash = v.to_string();
        } else if let Some(v) = message.strip_prefix("set-order=") {
            set_order = v.to_string();
        } else if let Some(v) = message.strip_prefix("sorted=") {
            sorted = v.to_string();
        }
    }
    (hash, set_order, sorted)
}

#[test]
fn rebuilding_a_python_rule_can_change_its_behavior() {
    let Some(a) = artifact("LANEKEEP_PY_DETERMINISM_A") else {
        return;
    };
    let Some(b) = artifact("LANEKEEP_PY_DETERMINISM_B") else {
        return;
    };

    let (hash_a, set_a, sorted_a) = observables(&a);
    let (hash_b, set_b, sorted_b) = observables(&b);

    assert!(!hash_a.is_empty(), "the probe reported a hash");
    assert!(!set_a.is_empty(), "the probe reported a set order");
    assert!(!sorted_a.is_empty(), "the probe reported a sorted order");

    // The hazard: the hash seed differs per build, and set iteration follows it.
    assert_ne!(hash_a, hash_b, "the hash seed differs per build");
    assert_ne!(set_a, set_b, "set iteration order differs per build");

    // The mitigation: sorted() is stable across builds.
    assert_eq!(sorted_a, sorted_b, "sorted() is stable across builds");
}
