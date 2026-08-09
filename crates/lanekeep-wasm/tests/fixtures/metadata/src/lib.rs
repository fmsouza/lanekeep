//! A guest that answers `metadata`, and nothing else.
//!
//! Every field carries a distinct value, so a host that transposed two of them — `message`
//! for `remediation`, say — fails rather than passing on a plausible-looking answer. The
//! other four exports are stubs: a WIT world has no optional exports, so every component
//! answers all of them regardless of which passes it uses, and this one is not shaped like a
//! real rule — see `tests/fixtures/engine-rule/` for that.

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
                path_matches: Vec::new(),
                file_contains: vec!["call".to_owned()],
            },
            timeout: None,
        }
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
