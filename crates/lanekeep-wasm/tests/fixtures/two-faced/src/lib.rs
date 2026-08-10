//! One component, one rule, and two different answers about what that rule is called.
//!
//! `world rule` splits `rules` from `metadata` deliberately: a rule's id has to be knowable
//! before the rule is configured, because the id is how a config names it, and `metadata` is
//! read after `configure`. Two exports therefore answer the same question, and until something
//! compared them a guest could answer it differently each time.
//!
//! **What that costs, if nobody checks.** `lanekeep-config` registers a slot under the id
//! `rules()` reported and builds the `RuleSpec` from `metadata()`. A guest where the two
//! disagree gets a rule that *runs* under one name and is *reported* under another — so the id
//! a user is shown in a violation is not the id a suppression comment or a `--rule` filter has
//! to name, and neither of those fails loudly. Every other fixture in this directory answers
//! consistently, which is exactly why none of them can catch a host that never asked.
//!
//! Everything else here is the smallest thing that satisfies the world. There is one rule; it
//! reports nothing; `check` and `reduce` succeed. The disagreement is the whole fixture.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// What `rules()` announces. A config that named this component registers a slot under it.
const ENUMERATED: &str = "fixture/enumerated";

/// What `metadata()` calls the same rule. Everything downstream of the description sees this.
const DESCRIBED: &str = "fixture/described";

struct Component;

impl Guest for Component {
    fn rules() -> Vec<String> {
        vec![ENUMERATED.to_owned()]
    }

    fn configure(_rule: u32, _options_json: String) -> Result<(), String> {
        Ok(())
    }

    fn metadata(_rule: u32) -> RuleMetadata {
        RuleMetadata {
            // The disagreement, and it is the only thing this fixture exists for.
            id: DESCRIBED.to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: "two-faced".to_owned(),
                remediation: "answer the same id twice".to_owned(),
                examples: RuleExamples {
                    bad: "bad()".to_owned(),
                    good: "good()".to_owned(),
                },
            },
            query: "(call_expression) @call".to_owned(),
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    fn has_check(_rule: u32) -> bool {
        true
    }

    fn has_reduce(_rule: u32) -> bool {
        false
    }

    /// Never reached: the host refuses this component while describing it, which is before any
    /// file is checked. It is here because a WIT world has no optional exports.
    fn check(_rule: u32, _ctx: &CheckContext, _m: Match) -> Result<(), RuleError> {
        Ok(())
    }

    fn reduce(_rule: u32, _ctx: &ReduceContext) -> Result<(), RuleError> {
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
