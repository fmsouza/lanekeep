//! `lanekeep/duplicate-implementation`'s options, through `RuleTester`.
//!
//! The reduce-phase behavior is exercised by `lanekeep-cli`'s corpus tests, which build a
//! whole project. What only `RuleTester::configured` can reach is the option plumbing: the
//! factory closes `minNodes` over, and a rule whose options were ignored would look exactly
//! like a rule whose threshold happened to be right. Both directions are asserted — raising
//! silences a pair the default reports, and lowering reports a pair the default silences.
//!
//! The two copies live in the one subject file rather than in a fixture: `RuleTester::run`
//! wipes `subject/` before each case, so any file a `write_fixture` wrote there would be
//! gone before the rule ran. Same-file duplicates group exactly like cross-file ones —
//! grouping is by fingerprint hash alone — so the option plumbing is tested just as well.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

/// A body comfortably above the default threshold (≈ 60 fingerprint nodes).
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

/// A body far below it (≈ 8 fingerprint nodes).
const SMALL_FN: &str = "\
function tiny() {
  return 1
}
";

/// Two identical bodies, either size — the second is the same shape under a new name.
fn pair(body: &str) -> String {
    let mut out = String::from(body);
    out.push_str(
        &body
            .replace("function compute", "function second")
            .replace("function tiny", "function tiny2"),
    );
    out
}

fn tester(min_nodes: u32) -> RuleTester {
    let source = lanekeep_rules::source("duplicate-implementation").expect("the rule ships");
    RuleTester::configured(
        "duplicate-implementation",
        source,
        &format!("{{ minNodes: {min_nodes} }}"),
    )
    .expect("builds")
    .with_builtins(lanekeep_rules::source)
}

#[test]
fn min_nodes_reaches_the_rule_through_configured_options() {
    // Lowering reports a pair the default would silence…
    let lowered = tester(5);
    let violations = lowered.run(&pair(SMALL_FN)).expect("runs");
    assert_eq!(
        violations.len(),
        2,
        "two identical small bodies should group at minNodes 5"
    );

    // …and raising silences a pair the default would report. The raise is the direction
    // that catches an ignored option: a rule that never read `minNodes` would still
    // report this pair.
    let raised = tester(200);
    let violations = raised.run(&pair(LARGE_FN)).expect("runs");
    assert!(
        violations.is_empty(),
        "a pair under minNodes 200 must stay silent, got {} violations",
        violations.len()
    );
}
