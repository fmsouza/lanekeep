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

use bindings::lanekeep::host::types::{QueryFor, RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    /// The one rule this component hosts. Every other export takes its index.
    fn rules() -> Vec<String> {
        vec!["fixture/wasip1".to_owned()]
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata(rule: u32) -> RuleMetadata {
        only(rule);
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

            queries: vec![QueryFor { language: "rust".to_owned(), query: String::new() }],
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
    fn configure(rule: u32, _options_json: String) -> Result<(), String> {
        only(rule);
        Err("fixture/wasip1 does not implement configure".to_owned())
    }

    fn has_check(rule: u32) -> bool {
        only(rule);
        true
    }

    fn has_reduce(rule: u32) -> bool {
        only(rule);
        false
    }

    fn check(rule: u32, ctx: &CheckContext, m: Match) -> Result<(), RuleError> {
        only(rule);
        // Two allocations and a format, so the artifact's import list is a measurement of
        // the target rather than of how little this guest does.
        let path = ctx.file_path();
        let names: Vec<&str> = m.iter().map(|entry| entry.name.as_str()).collect();
        ctx.report(
            ctx.root(),
            Some(&format!("{path}: {}", names.join(","))),
            None,
        );
        Ok(())
    }

    fn reduce(rule: u32, _ctx: &ReduceContext) -> Result<(), RuleError> {
        only(rule);
        Ok(())
    }
}

/// The one rule this component hosts.
///
/// A component hosts a *list* of rules and every export but `rules` takes an index into it.
/// This one hosts a single rule, so zero is the only index that answers — and a host asking
/// for another has disagreed with what `rules` reported, which is worth trapping on rather
/// than answering with the one rule's data under another rule's name.
fn only(rule: u32) {
    assert_eq!(rule, 0, "this component hosts one rule");
}

bindings::export!(Component with_types_in bindings);
