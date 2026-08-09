//! A guest shaped like a rule, for exercising `lanekeep-engine`'s component dispatch path.
//!
//! Every other fixture in this directory is a probe: it reports at the root and encodes what it
//! observed into the message, so a host can assert on one host-surface method at a time. This
//! one is deliberately not that. The engine's dispatch path has three jobs a probe cannot
//! exercise — carry a query match across the boundary, turn a report at a *captured* node into a
//! violation at the position the query found, and carry facts from the per-file pass into the
//! cross-file one — and each is invisible in a guest that reports at handle zero.
//!
//! It still asserts nothing for itself. A guest can only fail by trapping, and a trap says
//! nothing about which of three things was wrong, so everything observable is put where the host
//! can compare it.
//!
//! # What each pass does, and which engine-side mistake it is arranged to catch
//!
//! `check` reports at the node captured as `target`, with the node's own text in the message.
//! An engine that interned the capture paths into the wrong arena, dropped the capture names, or
//! reported at the root rather than at the node would produce a violation at 1:1 with an empty
//! text — a plausible-looking answer, which is why the text and the position are both in play.
//!
//! `reduce` reports every fact back **field by field**: `kind`, then `file`, then `data`. The
//! middle one is the whole point. Under QuickJS the emitting file is spliced into the payload,
//! because that engine's reduce fact carries no file of its own; the world's `emitted-fact` has
//! a `file` field, so a component host fills the field instead. An engine that did both would
//! send a `data` carrying a `"file"` key this guest never wrote — valid enough JSON for a parser
//! to accept and silently pick one of, and completely invisible from the host side, because the
//! host forwards `data` exactly as it was given. Reporting `data` verbatim is what makes it
//! visible.
//!
//! # Nothing here may index a slice
//!
//! A panic in a guest is a trap, and a trap aborts the call before any report crosses back — so
//! a wrong shape assumption would be reported as "the host recorded no violations", which is
//! also what a broken `report` looks like. Every lookup is a `find` or a `get`, with an explicit
//! `shape` report when the match is not what this file expects.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    FactError, ReduceLocation, RuleCard, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// The capture this rule reports at.
///
/// Named rather than positional, because "the first capture" and "the capture the query called
/// `target`" are different claims and only the second says the names crossed.
const TARGET: &str = "target";

/// The kind every fact is emitted under, so a `facts(Some(kind))` filter has something to select.
const KIND: &str = "seen";

/// A file this rule reads on every match, whether or not it is there.
///
/// The read exists for the *host* rather than for this guest: a tracked read becomes a
/// dependency of the checked file's cache entry, and the engine builds one `FileAccess` per file
/// and hands it to every rule on that file **in both engines**. If the component engine owned an
/// access of its own instead, this read would be recorded against a memo nothing reads back and
/// would vanish from the dependency list entirely. So the answer is deliberately ignored: what is
/// under test is that the read was recorded where the other engine's reads are.
const READS: &str = "shared.json";

/// A capture name asking this rule to spend real time before it reports.
///
/// A per-invocation budget can only be tested against a handler that actually burns bytecode.
/// `AGENTS.md` records why a fast one proves nothing: the budget is polled from compiled-in
/// epoch checks, so a handler that returns after a handful of operations is never asked to stop
/// — a fixture testing a timeout against one is testing nothing.
const BURN: &str = "burn";

/// Iterations of a loop the optimizer cannot remove, when [`BURN`] asks for it.
///
/// Sized so the loop takes far longer than the small budget one half of the timeout pair sets
/// and far less than the large budget the other half sets. Measured 2026-08-06, Apple M3 Max,
/// `wasmtime` 47.0.3: **about 1.1 seconds**, against a 20 ms lower budget (55×) and a one-hour
/// upper one (3,000×). The assertion is which budget was applied, and it must not become an
/// assertion about how fast the machine is.
const BURN_ITERATIONS: u64 = 400_000_000;

/// Somewhere the burn's result has to go, so nothing above it can be folded away.
static SINK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

struct Component;

impl Guest for Component {
    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata() -> RuleMetadata {
        RuleMetadata {
            id: "fixture/engine-rule".to_owned(),
            // Not `"rust"`: `crates/lanekeep-engine/src/lib.rs`'s `component_rule` is the only
            // place this fixture is actually driven, and it builds the real `RuleSpec` by hand
            // with `languages: vec!["typescript".to_owned()]` and the same query this guest
            // matches against. `metadata()` here is unexercised by any test, but this is the one
            // field with a real answer sitting elsewhere in the tree, so it agrees rather than
            // inventing a second one.
            languages: vec!["typescript".to_owned()],
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
        Err("fixture/engine-rule does not implement configure".to_owned())
    }

    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        true
    }

    fn check(ctx: &CheckContext, m: Match) {
        if m.iter().any(|entry| entry.name == BURN) {
            burn();
        }

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
            return;
        };

        // `text` rather than `kind`: a kind is the same for every match of one query, so an
        // engine that handed every invocation the same handle would still look right.
        let text = ctx
            .text(target)
            .unwrap_or_else(|| "<unresolved>".to_owned());
        ctx.report(target, Some(&format!("component saw `{text}`")), None);

        // Read, and ignore what came back — see [`READS`]. A *refusal* is reported, because a
        // read the host declined is a different thing from a file that is not there, and a
        // silent one would be indistinguishable from a memo that recorded nothing.
        if let Err(error) = ctx.read_file(READS) {
            ctx.report(target, Some(&format!("read: {error:?}")), None);
        }

        // No `file` key, deliberately — see the module header. The payload is built by hand
        // rather than serialized because this crate has no JSON library and the values are
        // identifiers from the test corpus.
        if let Err(error) = ctx.emit_fact(KIND, &format!("{{\"text\":\"{text}\"}}")) {
            ctx.report(target, Some(&format!("emit: {}", named(&error))), None);
        }
    }

    fn reduce(ctx: &ReduceContext) {
        for fact in ctx.facts(None) {
            // Reported *at the file the fact came from*, which is the only site a cross-file
            // rule has: the reduce phase never touches parse trees, so a position has to have
            // been captured while one was still there.
            ctx.report(
                &ReduceLocation {
                    file: fact.file.clone(),
                    line: Some(1),
                    column: Some(1),
                },
                Some(&format!("{}|{}|{}", fact.kind, fact.file, fact.data)),
            );
        }
    }
}

/// Spend real time, in a way nothing can elide.
///
/// The accumulator is written to a `static` at the end, so neither the loop nor the arithmetic
/// inside it is dead code. `#[inline(never)]` keeps it one function a reader can find in a
/// profile.
/// **`black_box`, and two weaker guards were measured failing first.** A linear congruential
/// step whose result is stored once at the end is strength-reduced to a closed form and the loop
/// disappears; storing into a `static AtomicU64` on *every* iteration does not save it either,
/// because `wasm32-unknown-unknown` has no atomics target feature, so `core::sync::atomic`
/// lowers to ordinary loads and stores and LLVM removes every store but the last. Both were
/// tried: a twenty-millisecond budget failed to notice four hundred million rounds of each.
///
/// `core::hint::black_box` is the documented way to say "this value is used" without saying how,
/// and it is what the `limits` fixture beside this one already uses for the same reason.
#[inline(never)]
fn burn() {
    let mut acc: u64 = 1;
    for i in 0..BURN_ITERATIONS {
        acc = core::hint::black_box(acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i));
    }
    SINK.store(acc, core::sync::atomic::Ordering::Relaxed);
}

/// A refusal, as a name the host can compare against.
fn named(error: &FactError) -> String {
    match error {
        FactError::EmptyKind => "empty-kind".to_owned(),
        FactError::NotAnObject => "not-an-object".to_owned(),
        FactError::InvalidJson(message) => format!("invalid-json({message})"),
    }
}

bindings::export!(Component with_types_in bindings);
