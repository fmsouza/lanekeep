//! `lanekeep/duplicate-implementation`, run through the real engine.
//!
//! A cross-file rule needs more than one file, so these build a project directly rather
//! than going through `RuleTester`, which runs one subject at a time.
//!
//! Fixture sizing matters and is deliberate: every body a test expects to group is well
//! above the default `minNodes` of 40 (measured in fingerprint nodes, which count kinds,
//! anonymous tokens and structure — a ten-line body is comfortably 50+), and every body a
//! test expects to stay silent is far below it. A "does not group" fixture is still above
//! the threshold, so its silence is proved to come from the shape difference and not from
//! being too small to participate.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

mod corpus;

use corpus::Corpus;

/// A body comfortably above the default `minNodes` threshold (≈ 60 fingerprint nodes).
const LARGE_FN: &str = "\
function compute(list) {
  const total = 0
  for (const item of list) {
    if (item.active) {
      total += item.value
    } else {
      total -= item.value
    }
  }
  return total
}
";

/// The same shape as [`LARGE_FN`] with every identifier and literal value changed — the
/// case the rule exists for.
const RENAMED_FN: &str = "\
function summarize(input) {
  const acc = 1
  for (const entry of input) {
    if (entry.ready) {
      acc += entry.score
    } else {
      acc -= entry.score
    }
  }
  return acc
}
";

/// A body far below the default threshold (≈ 8 fingerprint nodes).
const SMALL_FN: &str = "\
function tiny() {
  return 1
}
";

/// A class method and a block-bodied arrow, both above the threshold.
const METHODS_FN: &str = "\
class Widget {
  render(items) {
    const names = []
    const list = []
    for (const item of items) {
      if (item.enabled) {
        list.push(item.name)
      }
    }
    return list
  }
}

const summarize = (items) => {
  const list = []
  const seen = new Set()
  for (const item of items) {
    if (!seen.has(item)) {
      seen.add(item)
      list.push(item)
    }
  }
  return list
}
";

fn corpus(options: &str, files: &[(&str, &str)]) -> Corpus {
    Corpus::new("duplicate-implementation", options, files)
}

#[test]
fn two_identical_functions_in_different_files_report_both_sites() {
    let found = corpus("{}", &[("src/a.ts", LARGE_FN), ("src/b.ts", LARGE_FN)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/b.ts:1",
            "src/b.ts:1:1 'compute' duplicates the implementation at src/a.ts:1",
        ]
    );
}

#[test]
fn renamed_identifiers_and_changed_literal_values_still_group() {
    // The point of the rule: `summarize` is `compute` with different names and numbers,
    // and an agent that cannot see the whole corpus has already written it.
    let found = corpus("{}", &[("src/a.ts", LARGE_FN), ("src/b.ts", RENAMED_FN)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/b.ts:1",
            "src/b.ts:1:1 'summarize' duplicates the implementation at src/a.ts:1",
        ]
    );
}

#[test]
fn a_changed_operator_does_not_group() {
    // One `+=` becoming `-=` is a different body, whatever the two files look like side
    // by side. The fixture is above the threshold, so the silence is the operator's work.
    let b = LARGE_FN.replace("total += item.value", "total -= item.value");
    let found = corpus("{}", &[("src/a.ts", LARGE_FN), ("src/b.ts", &b)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_added_statement_does_not_group() {
    // A body with an extra statement has a different shape. Same threshold argument as
    // the operator case: both bodies are big enough to participate, and do not.
    let b = LARGE_FN.replace(
        "  const total = 0\n",
        "  const total = 0\n  const extra = item.weight\n",
    );
    let found = corpus("{}", &[("src/a.ts", LARGE_FN), ("src/b.ts", &b)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_comment_inside_the_body_does_not_hide_a_duplicate() {
    // Comments are erased before the body is fingerprinted, a docstring included — a
    // documented helper and its undocumented twin have the same shape and are flagged.
    // (A docstring is a comment in TypeScript, not a statement.)
    let b = LARGE_FN.replace(
        "  const total = 0\n",
        "  // total starts at zero\n  const total = 0\n",
    );
    let found = corpus("{}", &[("src/a.ts", LARGE_FN), ("src/b.ts", &b)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/b.ts:1",
            "src/b.ts:1:1 'compute' duplicates the implementation at src/a.ts:1",
        ]
    );
}

#[test]
fn a_group_of_three_names_its_counterparts_in_a_stable_order() {
    // Every member names the other two, sorted by (file, line), so the messages are the
    // same on every run — an unsorted or emission-order list would flake under parallel
    // checking.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", LARGE_FN),
            ("src/b.ts", LARGE_FN),
            ("src/c.ts", LARGE_FN),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/b.ts:1, src/c.ts:1",
            "src/b.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/c.ts:1",
            "src/c.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/b.ts:1",
        ]
    );
}

#[test]
fn many_counterparts_are_capped_in_the_message() {
    // A group of five names three counterparts and folds the rest, so the message stays
    // bounded however large the group grows.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", LARGE_FN),
            ("src/b.ts", LARGE_FN),
            ("src/c.ts", LARGE_FN),
            ("src/d.ts", LARGE_FN),
            ("src/e.ts", LARGE_FN),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/b.ts:1, src/c.ts:1, src/d.ts:1 and 1 more",
            "src/b.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/c.ts:1, src/d.ts:1 and 1 more",
            "src/c.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/b.ts:1, src/d.ts:1 and 1 more",
            "src/d.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/b.ts:1, src/c.ts:1 and 1 more",
            "src/e.ts:1:1 'compute' duplicates the implementation at src/a.ts:1, src/b.ts:1, src/c.ts:1 and 1 more",
        ]
    );
}

#[test]
fn small_bodies_stay_silent_below_the_default_threshold() {
    // Two identical two-line getters are noise, not a duplicate implementation. The
    // default of 40 keeps them out without anyone configuring anything.
    let found = corpus("{}", &[("src/a.ts", SMALL_FN), ("src/b.ts", SMALL_FN)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn min_nodes_is_asserted_in_both_directions() {
    // Raising the threshold must silence a pair the default reports, and lowering it must
    // report a pair the default silences. A test that only lowered it would pass against
    // a rule that ignored the option.
    let lowered = corpus(
        "{ minNodes: 5 }",
        &[("src/a.ts", SMALL_FN), ("src/b.ts", SMALL_FN)],
    )
    .run();
    assert_eq!(
        lowered,
        vec![
            "src/a.ts:1:1 'tiny' duplicates the implementation at src/b.ts:1",
            "src/b.ts:1:1 'tiny' duplicates the implementation at src/a.ts:1",
        ]
    );

    let raised = corpus(
        "{ minNodes: 200 }",
        &[("src/a.ts", LARGE_FN), ("src/b.ts", LARGE_FN)],
    )
    .run();
    assert_eq!(raised, Vec::<String>::new());
}

#[test]
fn method_definitions_and_block_arrow_functions_are_covered() {
    // Function declarations are not the only way an implementation is written twice.
    // Methods and block-bodied arrows fingerprint the same way.
    let found = corpus("{}", &[("src/a.ts", METHODS_FN), ("src/b.ts", METHODS_FN)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:2:3 duplicated implementation — also at src/b.ts:2",
            "src/a.ts:14:19 duplicated implementation — also at src/b.ts:14",
            "src/b.ts:2:3 duplicated implementation — also at src/a.ts:2",
            "src/b.ts:14:19 duplicated implementation — also at src/a.ts:14",
        ]
    );
}

#[test]
fn expression_bodied_arrows_are_not_matched() {
    // An expression-bodied one-liner is noise even when a matched body of the same size
    // would report — the threshold is lowered to 5 so a small block body does report,
    // which is what proves the expression pair's silence comes from the query, not from
    // being too small to participate.
    let file = "\
const pick = (x) => x.a.b.c + x.d.e.f
const block = (x) => { return x.a + x.b }
";
    let found = corpus("{ minNodes: 5 }", &[("src/a.ts", file), ("src/b.ts", file)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:2:15 duplicated implementation — also at src/b.ts:2",
            "src/b.ts:2:15 duplicated implementation — also at src/a.ts:2",
        ]
    );
}

#[test]
fn two_identical_functions_in_one_file_are_reported() {
    // Same-file duplicates are duplicates too — the file-agnostic grouping reports every
    // member of a group, wherever the members sit.
    let mut both = String::from(LARGE_FN);
    both.push_str(&LARGE_FN.replace("function compute", "function second"));
    let found = corpus("{}", &[("src/a.ts", &both)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'compute' duplicates the implementation at src/a.ts:12",
            "src/a.ts:12:1 'second' duplicates the implementation at src/a.ts:1",
        ]
    );
}

#[test]
fn the_same_corpus_reports_the_same_thing_every_run() {
    let corpus = corpus(
        "{}",
        &[
            ("src/a.ts", LARGE_FN),
            ("src/b.ts", RENAMED_FN),
            ("src/c.ts", LARGE_FN),
        ],
    );
    let first = corpus.run();
    assert!(!first.is_empty(), "the fixture should report something");
    for attempt in 0..4 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
}
