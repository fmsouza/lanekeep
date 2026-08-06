//! The spike guest: implements `lanekeep:spike/spike`'s single export and nothing else.
//!
//! Deliberately trivial. What it exists to prove is not that arithmetic works, but that a
//! component built here has an import list of exactly the declared world — checkable with
//! `wasm-tools component wit` before a byte executes — and that `lanekeep-wasm` can load,
//! instantiate and call it.

#[allow(warnings)]
mod bindings;

use bindings::Guest;

struct Component;

impl Guest for Component {
    fn add(a: u32, b: u32) -> u32 {
        a.wrapping_add(b)
    }
}

bindings::export!(Component with_types_in bindings);
