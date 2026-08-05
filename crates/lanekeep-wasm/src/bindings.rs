//! The generated host and guest bindings for `wit/world.wit`.
//!
//! Almost nothing here is hand-written. [`wasmtime::component::bindgen!`] expands the `rule`
//! world into the Rust the rest of this crate implements against: a `Host*` trait per
//! resource with one method per WIT function, the record, variant and enum types, and a
//! [`Rule`] struct whose `call_*` methods invoke the four exports.
//!
//! **The generated shape is the authority, not any description of it.** If a later change
//! expects a name the macro does not produce, the change adapts.
//!
//! # The two context types are hand-written, and they have to be
//!
//! `check-context` and `reduce-context` are *host-implemented* resources: the engine owns
//! the context and lends it to the guest for the length of one call, which is the opposite
//! of the guest-owned resource every mistake in this area looks like. wasmtime asks the
//! embedder to name the Rust type each one is represented by, and when nothing does it emits
//! an **uninhabited** type — measured here, `match x {}` on it compiles. That is a
//! deliberate stop rather than a default: no value of an uninhabited type exists, so nothing
//! can be pushed into a [`wasmtime::component::ResourceTable`], and no export taking
//! `borrow<check-context>` can be called at all. [`CheckContext`] and [`ReduceContext`]
//! below are those representations.
//!
//! # Why the module wrapper, and why the suppression is on it rather than on the crate
//!
//! `bindgen!` writes no doc comments onto what it generates, and this crate's reason for
//! existing is to hand those generated types to its callers — so they are re-exported, which
//! makes every one publicly reachable and fires `missing_docs` twenty-two times. Privacy is
//! not an answer here the way it is in `tests/spike.rs`, where the generated names never
//! leave the test.
//!
//! The suppression therefore sits on the module holding the expansion and nothing else. A
//! crate-level `#![expect(missing_docs)]` would cover every hand-written item added to this
//! crate afterwards, silently, which is the drift the `warn` exists to catch.

/// The engine's per-file context, as the resource table stores it.
///
/// Empty, and that is the current state rather than the design. What it will hold — the node
/// arena, the tracked-read log, the date-read flag, the violation sink — is `src/host.rs`'s
/// to define, and this crate does not yet depend on the types those are built from. What it
/// already does is make the world runnable: a resource with no named representation is
/// uninhabited, and an uninhabited context cannot be lent to anything.
#[derive(Debug, Default)]
pub struct CheckContext;

/// The engine's cross-file context, as the resource table stores it.
///
/// Empty for the same reason [`CheckContext`] is. Its eventual contents are the file list
/// and the collected facts, and nothing that could reach a parse tree: the reduce phase
/// consumes facts and the file list and nothing else, which is what keeps cross-file rules
/// parallel and cacheable.
#[derive(Debug, Default)]
pub struct ReduceContext;

/// The macro expansion itself, walled off so the suppression it needs reaches nothing else.
#[expect(
    missing_docs,
    reason = "wasmtime's bindgen! emits no doc comments, and these types are re-exported"
)]
mod generated {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "rule",

        // The host-implemented half of the world. Naming a Rust type here is what turns
        // `check-context` from an uninhabited placeholder into something the engine can
        // create, put in a resource table and lend to a guest export.
        with: {
            "lanekeep:host/types@0.1.0.check-context": super::CheckContext,
            "lanekeep:host/types@0.1.0.reduce-context": super::ReduceContext,
        },

        // Every host method returns `wasmtime::Result<T>` rather than a bare `T`.
        //
        // This does not change the WIT and no guest can observe it: a WIT `result` is still
        // a value the rule handles, which is what `read-error` and `fact-error` are for. It
        // changes what the *host* can do about an input that should be impossible — a
        // handle that is not live in the table, an arena index out of range. Without it the
        // only ways out are to fabricate a plausible answer or to panic, and this workspace
        // denies `panic!` outside tests precisely because an engine that panics on a
        // malformed input has failed at its job. With it, the host returns `Err`, the call
        // traps, and the run reports rather than dies.
        imports: { default: trappable },
    });
}

pub use generated::Rule;
pub use generated::lanekeep::host::types;
