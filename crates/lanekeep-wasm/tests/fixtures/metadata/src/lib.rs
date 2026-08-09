//! A guest that answers `metadata` and `configure`, and nothing else.
//!
//! Every field carries a distinct value, so a host that transposed two of them — `message`
//! for `remediation`, say — fails rather than passing on a plausible-looking answer.
//! `configure` accepts an object or `null` and refuses anything else with its own message,
//! which is what `tests/metadata.rs`'s four `configure` tests drive — through `RuleSet::add`
//! and `WasmRuntime::rule`, because that is the only door there is: an instance is configured
//! on the way to being handed out. This guest is also what `lanekeep-config`'s own tests point
//! a `.wasm` reference at, so its `metadata` values are asserted from two crates. The other four exports
//! are stubs: a WIT world has no optional exports, so every component answers all of them
//! regardless of which passes it uses, and this one is not shaped like a real rule — see
//! `tests/fixtures/engine-rule/` for that.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{RuleCard, RuleExamples, RuleGates, RuleMetadata};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    fn metadata() -> RuleMetadata {
        RuleMetadata {
            id: "fixture/metadata".to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: "a fixture".to_owned(),
                remediation: "do the other thing".to_owned(),
                examples: RuleExamples {
                    bad: "bad()".to_owned(),
                    good: "good()".to_owned(),
                },
            },
            query: "(call_expression) @call".to_owned(),
            gates: RuleGates {
                path_matches: vec!["src/**/*.rs".to_owned()],
                path_not_matches: vec!["**/generated/**".to_owned()],
                file_contains: vec!["call".to_owned()],
                file_not_contains: vec!["skip".to_owned()],
            },
            timeout: Some(1500),
        }
    }

    /// Accepts an object or `null`; refuses anything else with its own message.
    ///
    /// `tests/metadata.rs` drives all three shapes: a well-formed object, `null` (the
    /// bare-reference case), and a JSON array, which is refused rather than trapped so the
    /// refusal carries a message a user can act on. The array case is driven twice — once
    /// through `rule` and once through `check` — because "a refusal is its own variant" and
    /// "nothing checks anything before it has been configured" are different claims.
    fn configure(options_json: String) -> Result<(), String> {
        if options_json == "null" {
            return Ok(());
        }
        if !options_json.starts_with('{') {
            return Err("expected an object".to_owned());
        }
        Ok(())
    }

    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        false
    }

    fn check(_ctx: &CheckContext, _m: Match) {}

    fn reduce(_ctx: &ReduceContext) {}
}

bindings::export!(Component with_types_in bindings);
