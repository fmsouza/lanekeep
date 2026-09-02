//! Direct measurement of what the split bought: a cold probe and a warm construction.
//!
//! `oracle.rs` rests on one claim — that [`TypeScriptSupport::probe`] is the expensive step
//! and [`TypeScriptOracle::new`] is cheap enough to pay once per query match rather than once
//! per run. The design spec's ~810 ns figure for `new` was a subtraction between two
//! separately timed loops rather than a timing of `new` itself; this bench times each
//! operation directly instead, which is the whole reason it exists.
//!
//! # Why this is not criterion
//!
//! Same reasoning as `lanekeep-engine/benches/corpus.rs`: this is a report a human reads
//! once, with no stored baseline to compare against and no distribution worth plotting, so a
//! statistics framework buys nothing over the standard library's own clock.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "A benchmark is test scaffolding that `clippy.toml`'s allow-*-in-tests does not \
              reach, and its whole output is a printed report."
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_types::{TypeScriptOracle, TypeScriptSupport};

/// Timed attempts per operation; the minimum of these is reported.
///
/// Best rather than mean, matching `corpus.rs`'s own `measure`: every source of noise on a
/// shared machine only ever adds, so the minimum is the closest reading to the operation's
/// own cost.
const ATTEMPTS: usize = 7;

/// Calls per attempt when timing [`TypeScriptSupport::probe`].
///
/// `probe` is the expensive operation — microseconds, per `oracle.rs`'s own documentation —
/// so a few thousand calls already keep `Instant::now`'s own resolution far below the noise
/// floor without making the bench slow.
const PROBE_ITERATIONS: u32 = 10_000;

/// Calls per attempt when timing [`TypeScriptOracle::new`].
///
/// `new` is the operation this bench exists to pin down, and the design expects it to be
/// cheap — hundreds of nanoseconds or less — so it needs far more calls per attempt than
/// `probe` before `Instant::now`'s own resolution stops dominating the total.
const NEW_ITERATIONS: u32 = 1_000_000;

fn main() {
    let source = "const amount: number = 1;";
    let tree = parse(source);

    let probe_cost = measure(PROBE_ITERATIONS, || {
        black_box(TypeScriptSupport::probe(black_box(&TypeScript)));
    });

    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let new_cost = measure(NEW_ITERATIONS, || {
        black_box(TypeScriptOracle::new(
            black_box(&support),
            black_box(&tree),
            black_box(source),
        ));
    });

    println!("\nlanekeep-types construction — one thread\n");
    println!("  {:<26} {:>10}", "operation", "ns/call");
    println!(
        "  {:<26} {:>10.1}",
        "TypeScriptSupport::probe",
        probe_cost.as_secs_f64() * 1e9
    );
    println!(
        "  {:<26} {:>10.1}",
        "TypeScriptOracle::new",
        new_cost.as_secs_f64() * 1e9
    );
}

/// Parse `source` with the TypeScript grammar.
fn parse(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the TypeScript grammar loads");
    parser.parse(source, None).expect("the source parses")
}

/// The best of [`ATTEMPTS`] timed loops of `iterations` calls, as a per-call duration.
///
/// Best rather than mean, for the reason [`ATTEMPTS`] documents. Dividing inside this
/// function rather than at each call site is what keeps a caller from forgetting to.
fn measure(iterations: u32, mut run: impl FnMut()) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let start = Instant::now();
        for _ in 0..iterations {
            run();
        }
        best = best.min(start.elapsed());
    }
    best / iterations
}
