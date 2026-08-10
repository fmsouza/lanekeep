//! One component, two rules, and a rule that fails without trapping.
//!
//! The fixture the generalized world exists for. Every other guest in this directory hosts a
//! single rule, so none of them can tell a host that dispatches on the rule index from one that
//! ignores it: both answer identically when there is only ever index zero. Here the two rules
//! disagree about everything a host can read — their ids, their queries, their card messages and
//! the options each was handed — so a host that collapsed them, or transposed them, fails rather
//! than passing on a plausible-looking answer.
//!
//! # What each rule is arranged to catch
//!
//! `metadata` and `query` differ per rule, so a host reading one rule's description for another
//! is visible. `configure` writes the `tag` it was given into that rule's own slot and `metadata`
//! reads it back, so a guest that kept one shared tag — or a host that configured one instance
//! twice — shows up as both rules reporting the last tag written.
//!
//! Rule 1 fails deliberately when its match carries a capture named `boom`, through the world's
//! `rule-error` rather than by panicking. That is the distinction the channel was added for:
//! measured 2026-08-10, an uncaught JavaScript throw inside a component reaches the host as
//! `wasm trap: wasm 'unreachable' instruction executed` with the message, the type and the stack
//! all gone. A Rust guest has no stack of its own to hand over, so the frames here are written
//! by hand — what is under test is that a message and a frame list *cross the boundary at all*,
//! which is what a later source-map remapping is built on.
//!
//! # Nothing here may index a slice
//!
//! A panic in a guest is a trap, and a trap aborts the call before anything crosses back — so a
//! wrong shape assumption would be reported as "the host recorded no violations", which is also
//! what a broken `report` looks like. Every lookup is a `get` or an iterator.

#[allow(warnings)]
mod bindings;

use std::cell::RefCell;

use bindings::lanekeep::host::types::{
    RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata, StackFrame,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// Every rule this component hosts, by id, in the order `rules` reports them.
const RULE_IDS: &[&str] = &["fixture/first", "fixture/second"];

/// The capture name that makes rule 1 fail.
const BOOM: &str = "boom";

thread_local! {
    /// The `tag` each rule was configured with, by index.
    ///
    /// One slot per rule rather than one shared value, which is the whole point: a component
    /// hosting several rules has to keep their configurations apart, and a guest that did not
    /// would report the last tag written for both of them.
    static TAGS: RefCell<Vec<String>> = RefCell::new(RULE_IDS.iter().map(|_| String::new()).collect());
}

struct Component;

impl Guest for Component {
    fn rules() -> Vec<String> {
        RULE_IDS.iter().map(|id| (*id).to_owned()).collect()
    }

    /// Reads a `tag` out of the options and remembers it against this rule alone.
    ///
    /// The extraction is by hand because this crate has no JSON library and the value is a bare
    /// identifier the host wrote. `null` — the shape the world gives a rule named with no
    /// options — leaves the tag empty rather than being refused, so the fixture can be
    /// instantiated without being configured.
    fn configure(rule: u32, options_json: String) -> Result<(), String> {
        let index = index_of(rule)?;
        if options_json == "null" {
            return Ok(());
        }
        if !options_json.starts_with('{') {
            return Err("expected an object".to_owned());
        }
        let tag = tag_in(&options_json).unwrap_or_default();
        TAGS.with_borrow_mut(|tags| {
            if let Some(slot) = tags.get_mut(index) {
                *slot = tag;
            }
        });
        Ok(())
    }

    fn metadata(rule: u32) -> RuleMetadata {
        let Ok(index) = index_of(rule) else {
            // No error channel on this export, so a host asking about a rule this component
            // never listed is a disagreement that has to be loud. Answering with rule 0's
            // description would be the plausible-looking wrong answer.
            unreachable!("no rule at index {rule}");
        };
        let tag = TAGS.with_borrow(|tags| tags.get(index).cloned().unwrap_or_default());
        let id = RULE_IDS.get(index).copied().unwrap_or_default();

        RuleMetadata {
            id: id.to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                // The configured tag, echoed back. `tests/ruleset.rs` reads it here rather
                // than through a channel of its own, because `metadata` is the one export
                // whose answer is allowed to depend on `configure`.
                message: format!("{id} tag={tag}"),
                remediation: format!("{id} remediation"),
                examples: RuleExamples {
                    bad: format!("{id} bad"),
                    good: format!("{id} good"),
                },
            },
            // Distinct per rule, so two rules collapsing into one description is visible in a
            // field that has nothing to do with the tag.
            query: format!("(call_expression) @{}", index),
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    fn has_check(rule: u32) -> bool {
        index_of(rule).is_ok()
    }

    /// Neither rule has a cross-file pass. Every export is mandatory because a WIT world has no
    /// optional ones.
    fn has_reduce(_rule: u32) -> bool {
        false
    }

    fn check(rule: u32, ctx: &CheckContext, m: Match) -> Result<(), RuleError> {
        let index = index_of(rule).map_err(failure)?;

        // Rule 1 alone, and only for the `boom` capture: rule 0 handed the same match must
        // still succeed, or the test could not tell "this component fails" from "this rule
        // fails".
        if index == 1 && m.iter().any(|entry| entry.name == BOOM) {
            return Err(RuleError {
                message: "deliberate failure from a rule".to_owned(),
                frames: vec![
                    // Innermost first, and hand-written: a Rust guest has no stack to offer,
                    // and what is under test is the channel rather than the walk.
                    StackFrame {
                        function: BOOM.to_owned(),
                        file: "two-rules.rs".to_owned(),
                        line: 1,
                        column: 1,
                    },
                    StackFrame {
                        function: "check".to_owned(),
                        file: "two-rules.rs".to_owned(),
                        line: 2,
                        column: 1,
                    },
                ],
            });
        }

        let names: Vec<&str> = m.iter().map(|entry| entry.name.as_str()).collect();
        let id = RULE_IDS.get(index).copied().unwrap_or_default();
        ctx.report(
            ctx.root(),
            Some(&format!("{id}: {}", names.join(","))),
            None,
        );
        Ok(())
    }

    fn reduce(rule: u32, _ctx: &ReduceContext) -> Result<(), RuleError> {
        index_of(rule).map(|_| ()).map_err(failure)
    }
}

/// The position in [`RULE_IDS`] a rule index names, or the id of the fault.
fn index_of(rule: u32) -> Result<usize, String> {
    usize::try_from(rule)
        .ok()
        .filter(|index| *index < RULE_IDS.len())
        .ok_or_else(|| format!("no rule at index {rule}"))
}

/// A refusal, in the shape the world's graceful channel takes.
fn failure(message: String) -> RuleError {
    RuleError {
        message,
        frames: Vec::new(),
    }
}

/// The value of a `"tag"` key in a flat JSON object, without a JSON library.
///
/// Deliberately crude — the host writes `{"tag":"alpha"}` and nothing else — and written with
/// `split_once` rather than index arithmetic so there is no slicing here that could panic on a
/// boundary this function got wrong.
fn tag_in(options_json: &str) -> Option<String> {
    let (_, rest) = options_json.split_once("\"tag\"")?;
    let (_, rest) = rest.split_once(':')?;
    let (_, rest) = rest.split_once('"')?;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_owned())
}

bindings::export!(Component with_types_in bindings);
