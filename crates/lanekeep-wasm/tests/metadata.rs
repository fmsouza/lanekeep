//! The `metadata` export: what a component says about itself.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests reaches neither the helpers below nor a \
              `tests/` integration crate."
)]

mod common;

use common::runtime_for;

#[test]
fn a_component_declares_its_own_identity() {
    let (mut runtime, slot) = runtime_for("metadata");

    let declared = runtime.metadata(slot).expect("the export answers");

    assert_eq!(declared.id, "fixture/metadata");
    assert_eq!(declared.languages, vec!["rust".to_owned()]);
    assert_eq!(declared.severity, "error");
    assert_eq!(declared.query, "(call_expression) @call");
    assert_eq!(declared.card.message, "a fixture");
    assert_eq!(declared.card.remediation, "do the other thing");
    assert_eq!(declared.card.examples.bad, "bad()");
    assert_eq!(declared.card.examples.good, "good()");
    assert_eq!(declared.gates.file_contains, vec!["call".to_owned()]);
    assert!(declared.gates.path_matches.is_empty());
    assert_eq!(declared.timeout, None);
}
