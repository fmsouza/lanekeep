//! A guest that breaches every limit the runtime enforces, and two that do not.
//!
//! **It is a probe, not a rule and not an assertion**, on the terms
//! `tests/fixtures/facts/src/lib.rs` sets out: the host picks a probe by the first capture's
//! name and asserts on what came back. Two of the four probes here never return at all, so
//! they cannot report anything — what the host asserts on for those is the error.
//!
//! # Why every loop carries a `black_box`
//!
//! An empty `loop {}` and a `for` loop with no observable effect are both things a compiler
//! may legally reduce to nothing, and a probe that was optimized away would make the runtime
//! look as though it stopped a rule it never actually ran. `std::hint::black_box` is the
//! documented way to say "this value is used" without saying how, and it is what makes these
//! four probes measurements rather than hopes. The same reasoning applies to `grow`'s
//! allocations: without the `black_box`, nothing keeps the blocks alive and the allocator can
//! hand back the same page forever.
//!
//! # `work` is the one that exists because a cheap rule proves nothing
//!
//! `AGENTS.md` records that four hundred files against a one-line rule ran to completion under
//! a one-millisecond budget, and that a fixture testing a budget needs a rule that burns real
//! bytecode or it is testing nothing. `tick` is the cheap rule — deliberately, because the gap
//! it exposes is the point — and `work` is its control: the same host, the same budget, a guest
//! that keeps executing. Without the pair, a run that finishes cannot be told apart from a
//! limit that was never wired.

#[allow(warnings)]
mod bindings;

use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        true
    }

    fn check(ctx: &CheckContext, m: Match) {
        // Every entry after the first is an argument, by position rather than by name, on the
        // terms `tests/fixtures/facts/src/lib.rs` sets out: these are not query captures, they
        // are a list of strings the host chose and the order is the whole content.
        let args: Vec<&str> = m.iter().skip(1).map(|entry| entry.name.as_str()).collect();

        match m.first().map_or("", |entry| entry.name.as_str()) {
            "spin" => spin(),
            "grow" => grow(),
            "tick" => say(ctx, "tick"),
            "work" => work(ctx, &args),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
    }

    /// The cross-file half of the same two runaway probes.
    ///
    /// `reduce` is handed nothing but the context, so the only host-chosen list of strings it
    /// can read is `files()` — the channel `tests/fixtures/reduce/` already uses. The first
    /// entry is the probe name.
    fn reduce(ctx: &ReduceContext) {
        let files = ctx.files();
        match files.first().map_or("", String::as_str) {
            "spin" => spin(),
            "grow" => grow(),
            _ => tell(ctx, "reduced"),
        }
    }
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// The cross-file equivalent, at a site no corpus contains.
///
/// A reduce report needs a line and a column or the host refuses it, and these reports are
/// about the run rather than about any file in it.
fn tell(ctx: &ReduceContext, message: &str) {
    ctx.report(
        &bindings::lanekeep::host::types::ReduceLocation {
            file: "<probe>".to_owned(),
            line: Some(1),
            column: Some(1),
        },
        Some(message),
    );
}

/// A rule that never terminates.
///
/// Nothing here can finish, so the only way this call ends is the runtime ending it. The
/// accumulator exists so the loop has a carried dependency the optimizer cannot break.
fn spin() {
    let mut n: u64 = 0;
    loop {
        n = std::hint::black_box(n.wrapping_add(1));
    }
}

/// A rule that allocates without bound.
///
/// Sixty-four kilobytes at a time, which is one WebAssembly page, so the number of
/// `memory.grow` calls the host's limiter sees is close to the number of iterations rather
/// than amortized away by the allocator's own doubling. The blocks are kept — a `Vec` that is
/// never read is a `Vec` the allocator may reuse — and the whole is passed through
/// `black_box` on every iteration so that neither the allocation nor the retention can be
/// elided.
fn grow() {
    let mut held: Vec<Vec<u8>> = Vec::new();
    loop {
        held.push(vec![0_u8; 64 * 1024]);
        std::hint::black_box(&held);
    }
}

/// A rule that does a host-chosen amount of real work and then reports.
///
/// The argument is a count in millions of iterations, so the host decides how long a call
/// takes without the guest having to be rebuilt. That is what lets one fixture serve both
/// sides of a budget test: a call that must breach and a call that must not.
///
/// It **always terminates**, which is the difference between this and `spin`, and the
/// difference matters for a test that expects a breach. A probe that could only be stopped by
/// the runtime turns "the limit did not fire" into a hang; this one turns it into a failed
/// assertion with a message naming what ran.
fn work(ctx: &CheckContext, args: &[&str]) {
    let millions: u64 = args.first().and_then(|arg| arg.parse().ok()).unwrap_or(1);
    let mut n: u64 = 0;
    for i in 0..millions.saturating_mul(1_000_000) {
        n = std::hint::black_box(n.wrapping_add(i));
    }
    say(ctx, &format!("worked {millions}m, sum {n}"));
}

bindings::export!(Component with_types_in bindings);
