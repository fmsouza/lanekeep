//! A guest declaring one query per language, across grammars with different vocabulary.
//!
//! `metadata` answers two languages and two queries — `(call_expression) @target` for
//! TypeScript, `(call) @target` for Python — and `check` reports at whatever the query
//! captured. The interesting work is all host-side: the engine has to compile each
//! language's own query against that grammar and hand this guest matches from both, which
//! is exactly what a single-language fixture can never make visible.
//!
//! `check` reports with no message of its own, so a violation carries the card's — the
//! same arrangement the QuickJS sibling test uses, keeping the two engines' expected
//! output identical in shape.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    QueryFor, RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// The capture this rule reports at, in both languages' queries.
const TARGET: &str = "target";

struct Component;

impl Guest for Component {
    /// The one rule this component hosts. Every other export takes its index.
    fn rules() -> Vec<String> {
        vec!["fixture/polyglot".to_owned()]
    }

    fn metadata(rule: u32) -> RuleMetadata {
        only(rule);
        RuleMetadata {
            id: "fixture/polyglot".to_owned(),
            languages: vec!["typescript".to_owned(), "python".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: "called".to_owned(),
                remediation: "do not call things in this fixture".to_owned(),
                examples: RuleExamples {
                    bad: "f()".to_owned(),
                    good: "f".to_owned(),
                },
            },
            queries: vec![
                QueryFor {
                    language: "typescript".to_owned(),
                    query: "(call_expression) @target".to_owned(),
                },
                QueryFor {
                    language: "python".to_owned(),
                    query: "(call) @target".to_owned(),
                },
            ],
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    /// Accepts `null` and an object, exactly as `engine-rule` does, so a config can name
    /// this fixture bare or with options without the guest having an opinion.
    fn configure(rule: u32, options_json: String) -> Result<(), String> {
        only(rule);
        if options_json == "null" || options_json.starts_with('{') {
            return Ok(());
        }
        Err("fixture/polyglot expects an object or null".to_owned())
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
        let Some(target) = m
            .iter()
            .find(|entry| entry.name == TARGET)
            .map(|entry| entry.node)
        else {
            let names: Vec<&str> = m.iter().map(|entry| entry.name.as_str()).collect();
            ctx.report(
                ctx.root(),
                Some(&format!(
                    "shape: no `{TARGET}` capture, got [{}]",
                    names.join(", ")
                )),
                None,
            );
            return Ok(());
        };

        // No message, so the violation carries the card's — position is what the test
        // asserts, per language, and a fixed message keeps the two rows comparable.
        ctx.report(target, None, None);
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
