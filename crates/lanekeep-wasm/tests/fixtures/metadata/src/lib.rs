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
    ///
    /// # `"burn"` makes this the one fixture that can breach a config-load budget
    ///
    /// Config load is a phase that runs guest code under a clock of its own, and until this
    /// option existed nothing could test that clock. Every other budget fixture burns inside
    /// `check`, which config load never calls — and the calls it *does* make are microseconds,
    /// with compilation sitting outside the clock, so no budget small enough to be reached by
    /// them is expressible. A test for that phase therefore needs a guest that spends real time
    /// in `configure`, which is what this branch is.
    ///
    /// Opt-in by substring so that every existing caller is unaffected: `tests/metadata.rs`
    /// passes `null`, `{"allow":["a.rs"]}` and `[]`, none of which contains `"burn"`, and a
    /// fixture that got slower for everyone would be a poor trade for one test.
    fn configure(options_json: String) -> Result<(), String> {
        if options_json == "null" {
            return Ok(());
        }
        if !options_json.starts_with('{') {
            return Err("expected an object".to_owned());
        }
        if options_json.contains("\"burn\"") {
            burn();
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

/// Roughly a third of a second of guest work, against a budget a test sets far below it.
///
/// Sized for a ratio rather than a deadline. The breach case gives it ~50 ms and the raise case
/// gives it seconds, so the margin is more than an order of magnitude either way — which is what
/// keeps it honest on a loaded hosted runner, where a slower machine only makes the breach case
/// breach harder and leaves the raise case with room to spare.
const BURN_ITERATIONS: u64 = 300_000_000;

/// Somewhere the result has to go, or nothing above it survives the optimizer.
static SINK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Spend real time, in a way `wasm32-unknown-unknown` cannot optimize away.
///
/// **`black_box` inside the loop, and `AGENTS.md` records both weaker guards failing first.**
/// That target has no atomics feature, so `core::sync::atomic` lowers to ordinary loads and
/// stores and every write but the last is removed; and a linear congruential step whose result is
/// only stored at the end strength-reduces to a closed form and the loop disappears. A 20 ms
/// budget failed to notice four hundred million rounds of each. `core::hint::black_box` on every
/// iteration is what makes this a measurement rather than a hope, and it is what the `limits` and
/// `engine-rule` fixtures beside this one already use for the same reason.
#[inline(never)]
fn burn() {
    let mut acc: u64 = 1;
    for i in 0..BURN_ITERATIONS {
        acc = core::hint::black_box(acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i));
    }
    SINK.store(acc, core::sync::atomic::Ordering::Relaxed);
}

bindings::export!(Component with_types_in bindings);
