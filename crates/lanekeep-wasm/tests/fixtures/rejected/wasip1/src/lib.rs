//! A guest built for `wasm32-wasip1`, which is the wrong target, on purpose.
//!
//! Its source is unremarkable: it implements the same world every other fixture does and
//! does nothing interesting inside it. What makes it a fixture is the target it is built
//! for. `cargo component`'s default is `wasm32-wasip1`, and a component built there imports
//! a wall clock and two filesystem interfaces the moment the guest touches the parts of
//! `std` that reach the WASI adapter — precisely the ambient authority
//! `docs/architecture.md` §13 exists to withhold.
//!
//! **It allocates on purpose, and that is the whole fixture.** `AGENTS.md` records the trap:
//! a guest small enough to allocate nothing has *zero* imports on both targets, so a fixture
//! that does not reach `std` cannot tell a right target from a wrong one. Reading a `String`
//! back through the borrowed context and formatting it is what makes the difference visible.

#[allow(warnings)]
mod bindings;

use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        false
    }

    fn check(ctx: &CheckContext, m: Match) {
        // Two allocations and a format, so the artifact's import list is a measurement of
        // the target rather than of how little this guest does.
        let path = ctx.file_path();
        let names: Vec<&str> = m.iter().map(|entry| entry.name.as_str()).collect();
        ctx.report(ctx.root(), Some(&format!("{path}: {}", names.join(","))), None);
    }

    fn reduce(_: &ReduceContext) {}
}

bindings::export!(Component with_types_in bindings);
