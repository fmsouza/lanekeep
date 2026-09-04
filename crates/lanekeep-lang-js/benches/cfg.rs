//! What a CFG costs to build, measured directly.
//!
//! lanekeep #192 requires a stated budget for a function of a few hundred statements, and
//! `docs/architecture.md`'s §15 cold figure sits at ~0.65-0.72 s against an 800 ms target
//! — so there is no margin to spend on construction without knowing the number first.
//! Nothing in the engine calls `Cfg::build` yet; this exists so that #193, which will,
//! starts from a measurement rather than an assumption.
//!
//! # Why this is not criterion
//!
//! Same reasoning as `lanekeep-types/benches/construction.rs`: a report a human reads
//! once, with no stored baseline to compare against and no distribution worth plotting.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    clippy::format_push_string,
    reason = "A benchmark is test scaffolding that `clippy.toml`'s allow-*-in-tests does \
              not reach, and its whole output is a printed report. The string building in \
              `synthetic` runs once at setup, not in anything measured — optimizing it would \
              obscure the fixture for no gain, on the same terms `corpus.rs` sets out."
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use lanekeep_lang::Language;
use lanekeep_lang_js::{Cfg, TypeScript};
use tree_sitter::{Parser, Tree};

/// Timed attempts per size; the minimum is reported.
///
/// Best rather than mean, matching `construction.rs`: every source of noise on a shared
/// machine only ever adds, so the minimum is the closest reading to the operation's own
/// cost.
const ATTEMPTS: usize = 7;

/// Statement counts to report. The largest is #192's "a few hundred statements".
const SIZES: &[usize] = &[100, 200, 400];

fn main() {
    println!("CFG construction, minimum of {ATTEMPTS} attempts\n");
    println!(
        "{:>10}  {:>12}  {:>8}  {:>8}",
        "statements", "build", "blocks", "edges"
    );
    for &n in SIZES {
        let source = synthetic(n);
        let tree = parse(&source);
        let root = function(&tree);

        let cost = measure(|| {
            black_box(Cfg::build(black_box(&source), black_box(root)));
        });

        let cfg = Cfg::build(&source, root).expect("the fixture is a function");
        let blocks = cfg.blocks().count();
        let edges: usize = cfg.blocks().map(|(_, b)| b.successors.len()).sum();
        println!("{n:>10}  {cost:>12?}  {blocks:>8}  {edges:>8}");
    }
}

fn measure(mut operation: impl FnMut()) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        operation();
        best = best.min(started.elapsed());
    }
    best
}

/// The same fixture generator as `cfg_build.rs`'s test module, deliberately duplicated.
///
/// A bench is a separate crate target compiled without `cfg(test)`, so it cannot reach
/// `cfg::testing` at all. Sharing would mean making test scaffolding public API of a
/// published crate, which is a worse trade than twelve duplicated lines. Keep the two in
/// step: the budget test and this bench are only comparable while they build the same
/// input.
fn synthetic(n: usize) -> String {
    let mut source = String::from("function f(x) {\n");
    for i in 0..n {
        match i % 4 {
            0 => source.push_str(&format!("  const v{i} = x + {i};\n")),
            1 => source.push_str(&format!("  if (v{} > 0) {{ g({i}); }}\n", i - 1)),
            2 => source.push_str(&format!("  while (v{} > {i}) {{ h({i}); }}\n", i - 2)),
            _ => source.push_str(&format!("  const w{i} = a{i} && b{i};\n")),
        }
    }
    source.push_str("  return 0;\n}\n");
    source
}

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parser returns a tree");
    assert!(
        !tree.root_node().has_error(),
        "the synthetic fixture must parse"
    );
    tree
}

fn function(tree: &Tree) -> tree_sitter::Node<'_> {
    tree.root_node()
        .named_children(&mut tree.root_node().walk())
        .find(|node| node.kind() == "function_declaration")
        .expect("the fixture declares a function")
}
