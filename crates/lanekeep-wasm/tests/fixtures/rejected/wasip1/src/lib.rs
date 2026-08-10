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

use bindings::lanekeep::host::types::{RuleCard, RuleExamples, RuleGates, RuleMetadata};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata() -> RuleMetadata {
        RuleMetadata {
            id: "fixture/wasip1".to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: String::new(),
                remediation: String::new(),
                examples: RuleExamples {
                    bad: String::new(),
                    good: String::new(),
                },
            },
            query: String::new(),
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `configure` itself is tested.
    ///
    /// Refuses unconditionally rather than accepting anything, so a caller that reached this
    /// export on this fixture fails loudly instead of passing on a vacuous success.
    fn configure(_options_json: String) -> Result<(), String> {
        Err("fixture/wasip1 does not implement configure".to_owned())
    }

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
        ctx.report(
            ctx.root(),
            Some(&format!("{path}: {}", names.join(","))),
            None,
        );
    }

    fn reduce(_: &ReduceContext) {}
}

bindings::export!(Component with_types_in bindings);
