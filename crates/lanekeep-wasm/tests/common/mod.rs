//! Loading one committed fixture into a runtime, which every test here needs.
//!
//! `runtime_for`'s `.expect()` calls are covered by the crate-level `#![expect(...)]` in
//! whichever `tests/*.rs` file declares `mod common;` — this module has no crate root of its
//! own to carry one, and a second attribute here would leave that one unfulfilled.

use std::sync::Arc;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_wasm::{ComponentLoader, RuleSet, RuleSlot, WasmEngine, WasmRuntime};

/// A runtime holding exactly the named fixture, and its slot.
pub(crate) fn runtime_for(name: &str) -> (WasmRuntime, RuleSlot) {
    let engine = WasmEngine::new().expect("an engine");
    let bytes = std::fs::read(format!("tests/fixtures/{name}.wasm")).expect("the artifact ships");
    let loaded = ComponentLoader::without_cache()
        .load(&engine, name, &bytes)
        .expect("imports are permitted");

    let mut set = RuleSet::new(&engine).expect("a set");
    let slot = set.add(name, &loaded).expect("added");

    let limits = Limits::default();
    let clock = RunClock::start(limits.global_timeout);
    let runtime = WasmRuntime::for_rules(engine, Arc::new(set), limits, clock);
    (runtime, slot)
}
