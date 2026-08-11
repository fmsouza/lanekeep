//! Loading one committed fixture into a runtime, which every test here needs.
//!
//! `runtime_for`'s `.expect()` calls are covered by the crate-level `#![expect(...)]` in
//! whichever `tests/*.rs` file declares `mod common;` — this module has no crate root of its
//! own to carry one, and a second attribute here would leave that one unfulfilled.

// Each `tests/*.rs` is its own crate and uses the part of this module it needs, so an item
// unused by one of them is not dead code — it is code another one uses. `allow` rather than
// `expect`, because `expect` would be unfulfilled in whichever crate happens to use all of it.
#![allow(
    dead_code,
    reason = "one module shared by several test crates, each using the part it needs"
)]

use std::sync::Arc;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types::{MatchEntry, RuleMetadata};
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::{
    ComponentLoader, Loaded, RuleSet, RuleSlot, WasmEngine, WasmError, WasmRuntime,
};

/// A runtime holding exactly the named fixture, and its slot.
///
/// The rule is registered with `"null"` options, which is the shape the world gives a rule
/// named with no options. Nothing is instantiated yet: `WasmRuntime::rule` does that, and
/// configures what it built, on first use.
pub(crate) fn runtime_for(name: &str) -> (WasmRuntime, RuleSlot) {
    runtime_for_options(name, "null")
}

/// The same, with the options the rule will be configured with.
pub(crate) fn runtime_for_options(name: &str, options_json: &str) -> (WasmRuntime, RuleSlot) {
    let engine = WasmEngine::new().expect("an engine");
    let loaded = load(&engine, name);

    let mut set = RuleSet::new(&engine).expect("a set");
    // Rule 0: the fixtures this helper serves host exactly one rule each.
    let slot = set.add(name, &loaded, 0, options_json).expect("added");

    let limits = Limits::default();
    let clock = RunClock::start(limits.global_timeout);
    let runtime = WasmRuntime::for_rules(engine, Arc::new(set), limits, clock);
    (runtime, slot)
}

/// Every rule a fixture hosts, each in its own slot, in the order the component enumerates
/// them.
///
/// The enumeration is the component's own `rules` export, which is why this instantiates
/// before it builds the set it returns: `RuleSet::add` takes an index and cannot discover one,
/// since asking needs a store. That throwaway instance is dropped with its runtime — it exists
/// to answer one question, and answering it before configuration is the ordering the world's
/// `rules`/`metadata` split is for.
pub(crate) fn runtime_for_all(name: &str) -> (Ruleset, Vec<RuleSlot>) {
    let engine = WasmEngine::new().expect("an engine");
    let loaded = load(&engine, name);

    let limits = Limits::default();
    let mut probe = WasmRuntime::new(
        Arc::clone(&engine),
        limits,
        RunClock::start(limits.global_timeout),
    )
    .expect("a runtime with no rules in it");
    let instance = probe.instantiate(&loaded).expect("instantiates");
    let ids = probe.call_rules(&instance).expect("enumerates its rules");
    drop(probe);

    let mut harness = Ruleset {
        engine,
        loaded,
        options: ids.iter().map(|_| "null".to_owned()).collect(),
        ids,
        limits,
        runtime: None,
    };
    // From the set that issued them rather than counted out here: a slot is only meaningful
    // against the set that issued it, and every rebuild below adds the same rules in the same
    // order, so the slots stay the ones the caller was handed.
    let slots = harness
        .runtime()
        .expect("a set over the enumerated rules")
        .rules()
        .slots()
        .collect();
    (harness, slots)
}

/// A fixture's whole rule list, and the runtime executing it.
///
/// **`configure` is a method here and not on `WasmRuntime`, deliberately.** A rule's options
/// are recorded by `RuleSet::add` and applied by `WasmRuntime::rule` on the way to every
/// instance it builds, which is what makes "configured once, before any check, identically on
/// every worker" a property of the type rather than of whoever remembers to call something. A
/// runtime method that re-configured one worker's instance would put that back. So this
/// records the options and rebuilds the set, which is the door there is.
pub(crate) struct Ruleset {
    engine: Arc<WasmEngine>,
    loaded: Loaded,
    ids: Vec<String>,
    options: Vec<String>,
    limits: Limits,
    /// Built on demand and discarded whenever the options change.
    runtime: Option<WasmRuntime>,
}

impl std::fmt::Debug for Ruleset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ruleset")
            .field("ids", &self.ids)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Ruleset {
    /// Hand one rule its options, leaving every other rule's alone.
    ///
    /// Eager rather than deferred: the set is rebuilt and the slot's instance is built and
    /// configured now, so the guest's own answer is this call's answer rather than something
    /// that surfaces later at an unrelated assertion.
    pub(crate) fn configure(
        &mut self,
        slot: RuleSlot,
        options_json: &str,
    ) -> Result<(), WasmError> {
        if let Some(entry) = self.options.get_mut(slot.index()) {
            options_json.clone_into(entry);
        }
        self.runtime = None;
        self.runtime()?.rule(slot).map(|_| ())
    }

    /// What one rule says it is.
    pub(crate) fn metadata(&mut self, slot: RuleSlot) -> Result<RuleMetadata, WasmError> {
        self.runtime()?.metadata(slot)
    }

    /// Run one rule's per-file pass over a trivial file, with a single capture of the given
    /// name bound to the root.
    pub(crate) fn check_capture(&mut self, slot: RuleSlot, capture: &str) -> Result<(), WasmError> {
        let captures = vec![MatchEntry {
            name: capture.to_owned(),
            // The root's handle is 0. A rule reached through this helper is being asked what it
            // does with the *capture name*, not with the node.
            node: 0,
        }];
        let runtime = self.runtime()?;
        let context = check_context(runtime);
        runtime.check(slot, &context, &captures)
    }

    /// The runtime for the options recorded so far, building it if the options have moved.
    ///
    /// Reachable by a test, because a fixture whose observation is the *report* rather than the
    /// verdict has to take it off the runtime's host state, and `check_capture` above hands back
    /// only whether the call succeeded. Every slot this hands out belongs to the set this built,
    /// so a caller holding it across a `configure` — which discards the runtime — gets a fresh
    /// one on the next call rather than a stale borrow.
    pub(crate) fn runtime(&mut self) -> Result<&mut WasmRuntime, WasmError> {
        if self.runtime.is_none() {
            let mut set = RuleSet::new(&self.engine)?;
            for (index, (id, options)) in self.ids.iter().zip(&self.options).enumerate() {
                let index = u32::try_from(index).expect("a fixture has few enough rules");
                set.add(id, &self.loaded, index, options.clone())?;
            }
            let clock = RunClock::start(self.limits.global_timeout);
            self.runtime = Some(WasmRuntime::for_rules(
                Arc::clone(&self.engine),
                Arc::new(set),
                self.limits,
                clock,
            ));
        }
        self.runtime
            .as_mut()
            .ok_or_else(|| WasmError::Engine("the runtime was built and then vanished".to_owned()))
    }
}

/// The committed artifact, admitted by the loader's import check.
fn load(engine: &WasmEngine, name: &str) -> Loaded {
    let bytes = std::fs::read(format!("tests/fixtures/{name}.wasm")).expect("the artifact ships");
    ComponentLoader::without_cache()
        .load(engine, name, &bytes)
        .expect("imports are permitted")
}

/// A check context over one trivial file, so `check` has something to be given.
fn check_context(runtime: &mut WasmRuntime) -> lanekeep_wasm::Resource<CheckContext> {
    let source = "const x = 1;\n";
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
