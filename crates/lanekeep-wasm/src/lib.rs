//! WebAssembly component runtime for lanekeep rules.
//!
//! This crate owns the execution of rule components: the [`wasmtime::Engine`] they run on,
//! and — as later milestones land — the WIT host API they are linked against and the
//! precompiled artifacts they are loaded from. It sits beside `lanekeep-js` rather than
//! replacing it; QuickJS leaves the tree only once the last rule has migrated.
//!
//! The invariant it exists to protect is the sandbox's absence of ambient authority
//! (`docs/architecture.md` §13). Under QuickJS that absence is curated — intrinsics are
//! opted into and `Math.random` is *deleted*, which is weaker in principle because a
//! deletion can be undone if a reference escapes. A component imports exactly what the
//! host provides it, so there is nothing to delete and nothing to restore, and the
//! property is readable off the artifact with `wasm-tools component wit` before a byte
//! executes.
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
//! What the numbers do not bound: what a runtime that actually loads and instantiates
//! components costs, which is larger and which nobody has measured.

use wasmtime::{Config, Engine, Result};

/// Builds the [`Engine`] rule components execute on.
///
/// One `Engine` is shared across a run. It is the unit wasmtime caches compiled code in,
/// so building a second one would recompile everything the first already holds.
///
/// # Errors
///
/// Returns the error `wasmtime` reports when the configuration cannot be realized on this
/// host — a compiler backend the target does not support, or a memory tunable the platform
/// rejects.
pub fn engine() -> Result<Engine> {
    // Deliberately the default `Config` for now. The two tunables that must eventually be
    // set here — `memory_reservation` and `memory_guard_size` — are baked into every
    // precompiled artifact wasmtime writes and are therefore cache-key inputs, so they are
    // chosen where the cache key is defined rather than here.
    Engine::new(&Config::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_builds() {
        assert!(engine().is_ok());
    }
}
