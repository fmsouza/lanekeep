//! A guest that emits facts the host chose and reports exactly what came back.
//!
//! **It is a probe, not a rule and not an assertion**, on the terms
//! `tests/fixtures/reads/src/lib.rs` sets out: every observation is encoded into the message of
//! a `report`, and the host asserts on the recorded reports. A guest that asserted for itself
//! could only fail by trapping, and a trap says nothing about which observation was wrong.
//!
//! The probe to run is the name of the **first** capture in the `match` handed to `check`, and
//! every capture after it is an argument. `emit` takes a `kind` and a payload; `pairs` takes as
//! many `(kind, payload)` couples as the host cares to send and emits them in order.
//!
//! # The payloads come from the host, and that is load-bearing
//!
//! A malformed payload written into this file would be a constant only this crate knows. The
//! host-side test could then assert that *some* error came back but not that the message is the
//! parser's own — which is the assertion that catches a host inventing its own wording. Passing
//! the bytes in means the test names its own input and can compare against `serde_json` for it.
//!
//! It also makes one component cover every case: well-formed, empty kind, unterminated,
//! not-an-object, and any mixture of them in one invocation.
//!
//! # A refusal is reported, never unwrapped
//!
//! `emit-fact` returns a `result` whose error is the world's `fact-error`, and the whole reason
//! that channel exists is that a rule can *handle* a fact the host would not take. A probe that
//! unwrapped would turn every refusal into a trap and prove the opposite of the point, so every
//! call below is matched and rendered — including the successes, where an unexpected error is
//! reported rather than swallowed.
//!
//! `pairs` keeps going after a refusal and emits the couples that follow it, which is what
//! shows a refusal is a value: a rule told no about one fact is still running, and its later
//! facts are still recorded.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    FactError, RuleCard, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata() -> RuleMetadata {
        RuleMetadata {
            id: "fixture/facts".to_owned(),
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
                file_contains: Vec::new(),
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

    fn check(ctx: &CheckContext, m: Match) {
        // Every entry after the first is an argument. By position and not by name, because
        // these are not query captures: the host is passing a list of strings and the order it
        // passed them in is the whole content. Two payloads may legitimately be identical, and
        // a lookup by name could not tell them apart.
        let args: Vec<&str> = m.iter().skip(1).map(|entry| entry.name.as_str()).collect();

        match m.first().map_or("", |entry| entry.name.as_str()) {
            "emit" => emit(ctx, &args),
            "pairs" => pairs(ctx, &args),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
    }

    /// A check-only rule still exports `reduce`, because a WIT world has no optional exports.
    fn reduce(_ctx: &ReduceContext) {}
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// What `emit-fact` answered.
///
/// The case *and* its payload, because they fail independently: a host that mapped every
/// refusal onto one case would still carry a plausible message, and one that carried a message
/// of its own invention would still name the right case.
fn outcome(result: Result<(), FactError>) -> String {
    match result {
        Ok(()) => "ok".to_owned(),
        Err(FactError::EmptyKind) => "empty-kind".to_owned(),
        Err(FactError::NotAnObject) => "not-an-object".to_owned(),
        Err(FactError::InvalidJson(message)) => format!("invalid-json({message})"),
    }
}

/// One `emit-fact`, reported.
fn emit(ctx: &CheckContext, args: &[&str]) {
    let [kind, data] = args else {
        return say(
            ctx,
            &format!("shape: expected a kind and a payload, got {}", args.len()),
        );
    };
    say(ctx, &format!("emit={}", outcome(ctx.emit_fact(kind, data))));
}

/// Every `(kind, payload)` couple the host sent, in the order it sent them.
///
/// The loop does not stop at the first refusal, which is the part that shows a refusal is a
/// value rather than the end of the invocation: the couples after a rejected one are still
/// emitted, and the host asserts it saw a line for every one of them — and that only the
/// accepted ones were recorded.
fn pairs(ctx: &CheckContext, args: &[&str]) {
    if args.is_empty() || args.len() % 2 != 0 {
        return say(
            ctx,
            &format!("shape: expected couples, got {} arguments", args.len()),
        );
    }
    for couple in args.chunks(2) {
        let [kind, data] = couple else {
            return say(ctx, "shape: chunks(2) yielded a short couple");
        };
        say(ctx, &outcome(ctx.emit_fact(kind, data)));
    }
}

bindings::export!(Component with_types_in bindings);
