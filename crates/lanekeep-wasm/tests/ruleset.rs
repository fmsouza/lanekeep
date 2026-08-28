//! One component, two rules, dispatched independently.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

mod common;

#[test]
fn one_component_hosts_two_rules_with_independent_metadata() {
    let (mut runtime, slots) = common::runtime_for_all("two-rules");
    assert_eq!(slots.len(), 2, "the fixture declares two rules");

    let first = runtime.metadata(slots[0]).expect("metadata for rule 0");
    let second = runtime.metadata(slots[1]).expect("metadata for rule 1");

    assert_eq!(first.id, "fixture/first");
    assert_eq!(second.id, "fixture/second");
    assert_ne!(
        first.queries, second.queries,
        "two rules in one component must not collapse to one description"
    );
}

#[test]
fn configuring_one_rule_leaves_the_other_alone() {
    let (mut runtime, slots) = common::runtime_for_all("two-rules");
    runtime
        .configure(slots[0], r#"{"tag":"alpha"}"#)
        .expect("configures");
    runtime
        .configure(slots[1], r#"{"tag":"omega"}"#)
        .expect("configures");

    // The fixture echoes its configured tag into its card message.
    assert!(
        runtime
            .metadata(slots[0])
            .expect("md")
            .card
            .message
            .contains("alpha")
    );
    assert!(
        runtime
            .metadata(slots[1])
            .expect("md")
            .card
            .message
            .contains("omega")
    );
}
