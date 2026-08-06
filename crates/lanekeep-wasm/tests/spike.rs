//! Proof of existence: a component built in this repository loads, instantiates and
//! answers, on the feature set `lanekeep-wasm` actually ships.
//!
//! The guest is `tests/fixtures/spike/`, built with
//! `cargo component build --release --target wasm32-unknown-unknown` and committed as
//! `tests/fixtures/spike.wasm` — `just wasm-fixtures` rebuilds it. It is committed because
//! CI does not install `cargo component`, and because a fixture nobody can rebuild is
//! worse than a byte-identical one everybody can.
//!
//! **This load path is not the one that ships.** A real rule is loaded from a precompiled
//! `.cwasm` on a mapped file, and from an embedded byte slice only when no writable cache
//! directory exists: a slice cannot be mmapped, which forfeits wasmtime's copy-on-write
//! memory images at roughly 8× the resident memory per instance and 18% more instantiation
//! time. `include_bytes!` is fine *here*, for a fixture that is never shipped and never
//! precompiled.

use lanekeep_wasm::engine;
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

// In a module rather than at the top level, and that is load-bearing rather than tidy.
// wit-bindgen writes the world's types and methods with no doc comments, so at the top
// level of this crate they are publicly reachable and `missing_docs` fires on every one.
// Privacy answers it without a lint suppression, which a crate-level `expect` would not —
// that would also cover anything hand-written that followed.
mod bindings {
    wasmtime::component::bindgen!({
        path: "tests/fixtures/spike/wit",
        world: "spike",
    });
}

use bindings::Spike;

/// The component under test, as built by `just wasm-fixtures`.
const SPIKE: &[u8] = include_bytes!("fixtures/spike.wasm");

#[test]
fn a_component_loads_instantiates_and_answers() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, SPIKE).expect("the fixture is a valid component");
    let linker = Linker::new(&engine);
    let mut store = Store::new(&engine, ());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an unreachable deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX);

    let spike = Spike::instantiate(&mut store, &component, &linker).expect("instantiates");

    assert_eq!(
        spike
            .call_add(&mut store, 20, 22)
            .expect("the call returns"),
        42
    );
}

/// The sandbox invariant, asserted as behavior rather than as a claim.
///
/// Nothing is added to this `Linker`, deliberately: an empty linker can only satisfy a
/// component that imports nothing, so instantiating through one *is* the import check, and
/// a widened import list fails here naming the import that could not be resolved.
///
/// **What this does not prove is that the target is the reason.** Measured 2026-08-05 with
/// `cargo component` 0.21.1: this guest built for the default `wasm32-wasip1` target also
/// imports nothing, because `add` allocates nothing and touches no part of `std` that
/// reaches the WASI adapter. The same scaffold returning a `String` imports ten interfaces
/// on that target — among them `wasi:clocks/wall-clock`, `wasi:filesystem/types` and
/// `wasi:filesystem/preopens` — and zero on `wasm32-unknown-unknown`. So the default
/// target's danger is latent rather than visible: it looks clean on a guest this small and
/// grows a clock and a filesystem the moment a real rule formats a message. A per-fixture
/// assertion cannot catch that; only building on `wasm32-unknown-unknown` and checking
/// every artifact's import list at load can.
///
/// This is also weaker than the loader will eventually be, because it happens after the
/// component is compiled rather than before it is trusted.
#[test]
fn the_component_imports_nothing_at_all() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, SPIKE).expect("the fixture is a valid component");
    let empty = Linker::new(&engine);
    let mut store = Store::new(&engine, ());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an unreachable deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX);

    assert!(
        empty.instantiate(&mut store, &component).is_ok(),
        "an empty linker instantiated it, so the component's import list is empty"
    );
}
