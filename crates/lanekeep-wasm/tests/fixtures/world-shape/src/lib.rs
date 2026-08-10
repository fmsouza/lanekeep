//! A guest that implements every export of `lanekeep:host@0.1.0`'s `rule` world.
//!
//! Not a rule, and not a template for one. What it exists to prove is that the world as
//! written can be targeted at all — that a component can be built against it, that the two
//! host-implemented contexts arrive as borrowed handles, and that values cross in both
//! directions through the canonical ABI.
//!
//! **It allocates on purpose.** A guest that only returns integers imports nothing on
//! either build target, so an import-list assertion over one proves nothing about the
//! target being right; a guest one `String` different imports ten interfaces on
//! `wasm32-wasip1` and none on `wasm32-unknown-unknown`. Formatting a message and reading
//! a `list<string>` is what makes the difference visible here.

#[allow(warnings)]
mod bindings;

// `CheckContext`, `ReduceContext` and `Match` are at the top level of the generated
// bindings because the *world* `use`s them; `ReduceLocation` is not, because it does not
// appear in an export signature and the world does not name it. That is the whole rule for
// what a guest gets flat and what it reaches through the interface path, and it is worth
// knowing before sub-project 3 writes an authoring crate around it.
use bindings::lanekeep::host::types::{
    ReduceLocation, RuleCard, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata() -> RuleMetadata {
        RuleMetadata {
            id: "fixture/world-shape".to_owned(),
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

    /// Accepts the no-options shape and refuses everything else.
    ///
    /// It used to refuse unconditionally, on the reasoning that reaching an unimplemented
    /// export should fail loudly rather than pass vacuously. That reasoning is now wrong for
    /// this fixture and stays right for every sibling whose `configure` still answers `does not
    /// implement configure` — stated as a property rather than a count, because the count was
    /// wrong within two commits of being written: `WasmRuntime::rule` configures
    /// every instance it builds, so `null` is reached on the ordinary path — by
    /// `tests/instantiation.rs` and `tests/load.rs`, which drive this guest through a
    /// `RuleSet` — and refusing it would mean this fixture could not be instantiated at all.
    ///
    /// Refusing anything else keeps the loud half: a caller that hands this fixture real
    /// options is still told that it has no idea what to do with them.
    fn configure(options_json: String) -> Result<(), String> {
        if options_json == "null" {
            return Ok(());
        }
        Err("fixture/world-shape takes no options".to_owned())
    }

    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        true
    }

    fn check(ctx: &CheckContext, m: Match) {
        // Reads through the borrowed handle, so the host observes that the borrow is live
        // for the length of the call, and allocates, so the artifact's import list is a
        // real measurement rather than an artifact of the guest being too small.
        let path = ctx.file_path();
        let names: Vec<&str> = m.iter().map(|entry| entry.name.as_str()).collect();
        let node = m.first().map_or_else(|| ctx.root(), |entry| entry.node);
        ctx.report(
            node,
            Some(&format!("{path}: {}", names.join(","))),
            // No fix: `report`'s two optional parameters are independent, and passing one
            // without the other is the shape that could not be expressed as a union.
            None,
        );
    }

    fn reduce(ctx: &ReduceContext) {
        let files = ctx.files();
        let kinds: Vec<String> = ctx.facts(None).into_iter().map(|fact| fact.kind).collect();
        ctx.report(
            &ReduceLocation {
                file: files.first().cloned().unwrap_or_default(),
                // `none` for both, deliberately, and this is an ABI test rather than a
                // model of a real report. The record declares `option<u32>`, so absence
                // has to survive the canonical ABI and arrive at the host as `None` —
                // which is what this world's stub host asserts.
                //
                // The *real* host refuses this call. `lanekeep_wasm::host` fails a report
                // with no line or column: a cross-file violation with no site is
                // unactionable, and 1:1 cannot be told apart from a rule that meant 1:1.
                // The option is in the record because the published TypeScript
                // `ReduceLocation` has it, not because a positionless report works. See
                // `wit/world.wit`'s `reduce-location`, and `tests/reduce.rs`.
                line: None,
                column: None,
            },
            Some(&format!("{} files, {} facts", files.len(), kinds.len())),
        );
    }
}

bindings::export!(Component with_types_in bindings);
