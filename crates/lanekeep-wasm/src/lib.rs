//! WebAssembly component runtime for lanekeep rules.
//!
//! This crate owns the execution of rule components: the [`wasmtime::Engine`] they run on,
//! the WIT host API they are linked against, and the precompiled artifacts they are loaded
//! from. It sits beside `lanekeep-js` rather than replacing it; QuickJS leaves the tree only
//! once the last rule has migrated.
//!
//! Three modules and one order. [`load`] decides whether a component may run at all — by
//! reading its import list, before instantiation and independently of what the host happens
//! to link — and where its compiled form comes from, which is a mapped `.cwasm` in lanekeep's
//! own cache directory whenever there is one. [`runtime::RuleSet`] resolves each admitted
//! component against the host world once for the run. [`runtime::WasmRuntime`] is one store
//! per worker, holding one lazily built instance per rule.
//!
//! A fourth module answers to a different owner. [`key`] holds the two values a run folds into
//! its cache key before any of that happens: what a rule may reach, and what a component was
//! compiled against. Both are derived rather than declared — the first from `wit/world.wit`'s
//! own bytes, the second from the [`wasmtime::Engine`] [`runtime`] builds — because a
//! cache-key input somebody has to remember to update is the failure the whole cache design is
//! arranged against.
//!
//! # The boundary lives in `wit/world.wit`
//!
//! One file, `lanekeep:host@0.1.0`, holding one interface and one world. Rust, TypeScript
//! and Go rule authoring all generate their bindings from it, and [`bindings`] is this
//! crate's own generated view of the same source. It is the most expensive thing here to
//! get wrong: four sub-projects bind against its shape, and its bytes become a cache-key
//! input, so an edit invalidates every cached result in every checkout.
//!
//! Two things about it that a reader will otherwise assume the other way round. Both
//! phases' contexts live in **one** interface, so a per-file rule can *name* the cross-file
//! context even though it can never obtain one — the file itself carries the measurement
//! that settled that against a per-phase split, and the residual weakness in full. And what
//! a built component declares is a **subset** of what the world offers: the WIT embedded in
//! an artifact lists only the methods that guest actually calls, so nothing downstream may
//! compare an artifact's WIT against this crate's and expect equality.
//!
//! The invariant it exists to protect is the sandbox's absence of ambient authority
//! (`docs/architecture.md` §13). Under QuickJS that absence is curated — intrinsics are
//! opted into and `Math.random` is *deleted*, which is weaker in principle because a
//! deletion can be undone if a reference escapes. A component imports exactly what the
//! host provides it, so there is nothing to delete and nothing to restore, and the
//! property is readable off the artifact with `wasm-tools component wit` before a byte
//! executes.
//!
//! That property is a consequence of the build target, and it is not self-announcing.
//! Rule components are built for `wasm32-unknown-unknown`, whose import list is exactly
//! the declared world; `cargo component`'s default target is `wasm32-wasip1`, whose
//! components import a wall clock and two filesystem interfaces as soon as the guest
//! touches the parts of `std` that reach the WASI adapter. A guest small enough to touch
//! none of them looks identical on both targets, which is why the target is pinned at the
//! build and the import list is checked at load, rather than either one standing in for
//! the other.
//!
//! # The runtime configuration, recorded rather than inferred
//!
//! `wasmtime` **47.0.3**, with `default-features = false` and
//! `features = ["runtime", "component-model", "cranelift", "std"]`. No `async`: every
//! measurement behind this design ran on a plain synchronous `Store`, and the feature
//! would pull in a fiber-based executor for nothing.
//!
//! Two notes about that list, both of which cost the design spike time:
//!
//! - `cranelift` already implies `std` — wasmtime 47.0.3 defines
//!   `cranelift = ["dep:wasmtime-cranelift", "std", "wasmtime-unwinder/cranelift"]` — so
//!   naming `std` is redundant, not additive.
//! - Dropping `cranelift` *without* naming `std` selects wasmtime's `custom` platform
//!   backend, meant for bare-metal embedders that supply `wasmtime_tls_get` and related
//!   TLS primitives themselves. lanekeep does not, so such a build fails at the link step
//!   — and `cargo check` never invokes the linker, so it reports success on a
//!   configuration that cannot produce a binary. The redundant `std` is a guard against
//!   an edit that removes `cranelift` and passes a check it should not.
//!
//! `cranelift` is in the binary for a reason that is not speed of execution: lanekeep is
//! the only thing on a user's machine that can turn a rule component into a form a run can
//! afford to load. Compiling twenty components at check time costs about 186 ms against
//! about 0.74 ms to load twenty precompiled ones, and 186 ms is 23% of the entire 800 ms
//! cold budget in `docs/architecture.md` §15, spent before a file is read.
//!
//! # What linking it costs
//!
//! Measured in this workspace on 2026-08-05, Apple M3 Max, `aarch64-apple-darwin`,
//! `rustc`/`cargo` 1.95.0, with an `Engine` constructed and immediately dropped through
//! `std::hint::black_box` from a reachable call in `lanekeep-cli`'s `main` — no component
//! loaded, nothing executed. Both profiles strip symbols.
//!
//! | Profile | Without `wasmtime` | With `wasmtime` + `cranelift` | Delta |
//! |---|---|---|---|
//! | `release` (`lto = "thin"`) | 9,763,536 | 15,677,392 | **+5,913,856** |
//! | `dist` (`lto = "fat"`, what ships) | 9,425,712 | 15,235,440 | **+5,809,728** |
//!
//! The `release` delta lands 112 bytes — 0.002% — under the +5,913,968 the design spike
//! measured against its own throwaway baseline, so this workspace's dependency graph does
//! not change the answer. It refutes two written estimates that were never measured: the
//! design spec's "+15–25 MB" and the decision record's "40 MB+" headline, the latter by
//! about a factor of seven.
//!
//! The `dist` row settles something both of those documents left open as an expectation.
//! Fat LTO does *not* shave a similar fraction off the `wasmtime` delta as off the
//! baseline: the baseline shrinks 3.46% and the delta only 1.76%, so `cranelift`'s
//! contribution is roughly half as compressible as the code it is added to.
//!
//! What the numbers do not bound is what a runtime that actually loads and instantiates
//! components costs, which is larger. Part of that is now measured: [`runtime`]'s
//! `MEMORY_RESERVATION` carries instantiation, execution and boundary-crossing figures for
//! the two memory tunables, which turn out to trade against each other in opposite
//! directions and to be baked into every precompiled artifact.

pub mod bindings;
pub mod error;
pub mod host;
pub mod key;
pub mod load;
pub mod runtime;

// Private, and the visibility is the claim: nothing outside this crate validates a fact,
// because nothing outside it receives one from a guest. The module exists so the three
// answers can be pinned down without a store, a component and a parsed file, not so callers
// can reach them.
mod facts;

/// The handle a host context is held by while it is lent to a guest.
///
/// Re-exported because it is unavoidably part of this crate's surface —
/// [`runtime::WasmRuntime::check`] takes one, and [`host::HostState::push_check_context`]
/// hands one back — and an embedder should not have to take a direct `wasmtime` dependency to
/// name a type this crate already requires of it. `lanekeep-engine` is the first such caller.
pub use wasmtime::component::Resource;

pub use error::WasmError;
pub use key::{EXTERNAL_BINDINGS, ExternalBinding, compile_env_hash, host_api_hash};
pub use load::{ComponentLoader, LoadSource, Loaded, PermittedImports};
pub use runtime::{RuleSet, RuleSlot, WasmEngine, WasmRuntime, engine};
