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

// --- the other grammars ------------------------------------------------------------------
//
// The fingerprint is language-agnostic — a fold over node kinds with token text erased — so
// what these fixtures assert per language is the query: that each grammar's function forms
// reach the fingerprint at all. Every "groups" fixture is comfortably above the default
// `minNodes` of 40 and every "stays silent" fixture is far below it, on the same reasoning
// as the TypeScript fixtures above.

/// Python: same shape as [`LARGE_FN`], spelled in the python grammar.
const PY_FN: &str = "\
def compute(items):
    total = 0
    count = 0
    for item in items:
        if item.active:
            total += item.value
            count += 1
        else:
            total -= item.value
    return total
";

/// [`PY_FN`] with every identifier and literal value changed.
const PY_RENAMED: &str = "\
def summarize(entries):
    acc = 1
    seen = 2
    for entry in entries:
        if entry.ready:
            acc += entry.score
            seen += 3
        else:
            acc -= entry.score
    return acc
";

/// A small python body, far below the default threshold.
const PY_SMALL: &str = "\
def tiny():
    x = 1
    return x
";

/// Go: the same shape again. The functions sit at line 3, after the package clause.
const GO_FN: &str = "\
package main

func Compute(items []Item) int {
\ttotal := 0
\tcount := 0
\tfor _, item := range items {
\t\tif item.Active {
\t\t\ttotal += item.Value
\t\t\tcount += 1
\t\t} else {
\t\t\ttotal -= item.Value
\t\t}
\t}
\treturn total
}
";

/// [`GO_FN`] with every identifier and literal value changed.
const GO_RENAMED: &str = "\
package main

func Summarize(entries []Record) int {
\tacc := 1
\tseen := 2
\tfor _, entry := range entries {
\t\tif entry.Ready {
\t\t\tacc += entry.Score
\t\t\tseen += 3
\t\t} else {
\t\t\tacc -= entry.Score
\t\t}
\t}
\treturn acc
}
";

/// A small go body, far below the default threshold.
const GO_SMALL: &str = "\
package main

func Tiny() int {
\tx := 1
\treturn x
}
";

/// Rust: the same shape once more.
const RS_FN: &str = "\
fn compute(items: &[Item]) -> i32 {
    let mut total = 0;
    let mut count = 0;
    for item in items {
        if item.active {
            total += item.value;
            count += 1;
        } else {
            total -= item.value;
        }
    }
    total
}
";

/// [`RS_FN`] with every identifier and literal value changed.
const RS_RENAMED: &str = "\
fn summarize(entries: &[Record]) -> i32 {
    let mut acc = 1;
    let mut seen = 2;
    for entry in entries {
        if entry.ready {
            acc += entry.score;
            seen += 3;
        } else {
            acc -= entry.score;
        }
    }
    acc
}
";

/// A small rust body, far below the default threshold.
const RS_SMALL: &str = "\
fn tiny() -> i32 {
    let x = 1;
    x
}
";

#[test]
fn python_renamed_identifiers_and_changed_literal_values_still_group() {
    let found = corpus("{}", &[("src/a.py", PY_FN), ("src/b.py", PY_RENAMED)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.py:1:1 'compute' duplicates the implementation at src/b.py:1",
            "src/b.py:1:1 'summarize' duplicates the implementation at src/a.py:1",
        ]
    );
}

#[test]
fn python_a_changed_operator_does_not_group() {
    let b = PY_FN.replace("total += item.value", "total -= item.value");
    let found = corpus("{}", &[("src/a.py", PY_FN), ("src/b.py", &b)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn python_docstrings_are_statements_not_comments() {
    // In python a docstring is an expression statement, not a comment, so the two
    // directions pull apart — unlike TypeScript, where a doc comment is erased.
    //
    // Different docstrings still group: the string's text is erased and the shapes match.
    let with_one = PY_FN.replace(
        "    total = 0\n",
        "    \"\"\"Sum the active items.\"\"\"\n    total = 0\n",
    );
    let with_other = PY_RENAMED.replace(
        "    acc = 1\n",
        "    \"\"\"Entirely different words here.\"\"\"\n    acc = 1\n",
    );
    let found = corpus("{}", &[("src/a.py", &with_one), ("src/b.py", &with_other)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.py:1:1 'compute' duplicates the implementation at src/b.py:1",
            "src/b.py:1:1 'summarize' duplicates the implementation at src/a.py:1",
        ]
    );

    // With-docstring against without differs by a statement, so it does not group.
    let found = corpus("{}", &[("src/a.py", PY_FN), ("src/b.py", &with_one)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn go_renamed_identifiers_and_changed_literal_values_still_group() {
    let found = corpus("{}", &[("src/a.go", GO_FN), ("src/b.go", GO_RENAMED)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.go:3:1 'Compute' duplicates the implementation at src/b.go:3",
            "src/b.go:3:1 'Summarize' duplicates the implementation at src/a.go:3",
        ]
    );
}

#[test]
fn go_a_changed_operator_does_not_group() {
    let b = GO_FN.replace("total += item.Value", "total -= item.Value");
    let found = corpus("{}", &[("src/a.go", GO_FN), ("src/b.go", &b)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn go_a_method_groups_with_a_function_of_the_same_body() {
    // The fingerprint is rooted at the body, so a `method_declaration` and a
    // `function_declaration` with one shape are one implementation — which is what an
    // agent moving a helper onto a receiver produces.
    let method = GO_FN.replace("func Compute(", "func (w Widget) Compute(");
    let found = corpus("{}", &[("src/a.go", GO_FN), ("src/b.go", &method)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.go:3:1 'Compute' duplicates the implementation at src/b.go:3",
            "src/b.go:3:1 'Compute' duplicates the implementation at src/a.go:3",
        ]
    );
}

#[test]
fn rust_renamed_identifiers_and_changed_literal_values_still_group() {
    let found = corpus("{}", &[("src/a.rs", RS_FN), ("src/b.rs", RS_RENAMED)]).run();
    assert_eq!(
        found,
        vec![
            "src/a.rs:1:1 'compute' duplicates the implementation at src/b.rs:1",
            "src/b.rs:1:1 'summarize' duplicates the implementation at src/a.rs:1",
        ]
    );
}

#[test]
fn rust_a_changed_operator_does_not_group() {
    let b = RS_FN.replace("total += item.value", "total -= item.value");
    let found = corpus("{}", &[("src/a.rs", RS_FN), ("src/b.rs", &b)]).run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn min_nodes_applies_in_both_directions_in_every_language() {
    // The option has to reach every grammar's facts, not just the TypeScript ones — and
    // the raise is the direction that catches an ignored option, exactly as the
    // TypeScript test above says.
    for (ext, small, large) in [
        ("py", PY_SMALL, PY_FN),
        ("go", GO_SMALL, GO_FN),
        ("rs", RS_SMALL, RS_FN),
    ] {
        let a = format!("src/a.{ext}");
        let b = format!("src/b.{ext}");

        let lowered = corpus(
            "{ minNodes: 5 }",
            &[(a.as_str(), small), (b.as_str(), small)],
        )
        .run();
        assert_eq!(
            lowered.len(),
            2,
            "a small {ext} pair should group at minNodes 5"
        );

        let raised = corpus(
            "{ minNodes: 200 }",
            &[(a.as_str(), large), (b.as_str(), large)],
        )
        .run();
        assert_eq!(
            raised,
            Vec::<String>::new(),
            "a large {ext} pair must stay silent at minNodes 200"
        );
    }
}

#[test]
fn structurally_parallel_bodies_do_not_group_across_languages() {
    // The same algorithm spelled in four grammars: interior node kinds differ per grammar
    // (`attribute` against `selector_expression` against `field_expression`), so none of
    // these four bodies share a fingerprint. Grouping across languages would report a
    // "duplicate" nobody could deduplicate.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", LARGE_FN),
            ("src/b.py", PY_FN),
            ("src/c.go", GO_FN),
            ("src/d.rs", RS_FN),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_mixed_corpus_reports_the_same_thing_every_run() {
    // Determinism across grammars: every language's pair reports, sorted by
    // (file, line, column), identically on every run.
    let corpus = corpus(
        "{}",
        &[
            ("src/a.py", PY_FN),
            ("src/b.py", PY_RENAMED),
            ("src/c.go", GO_FN),
            ("src/d.go", GO_RENAMED),
            ("src/e.rs", RS_FN),
            ("src/f.rs", RS_RENAMED),
            ("src/g.ts", LARGE_FN),
            ("src/h.ts", LARGE_FN),
        ],
    );
    let first = corpus.run();
    assert_eq!(
        first.len(),
        8,
        "every language's pair should report: {first:?}"
    );
    for attempt in 0..4 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
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
