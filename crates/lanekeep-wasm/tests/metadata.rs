//! The `metadata` and `configure` exports: what a component says about itself, and the
//! options it accepts.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests reaches neither the helpers below nor a \
              `tests/` integration crate."
)]

mod common;

use common::runtime_for;
use lanekeep_wasm::WasmError;

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
    assert_eq!(declared.gates.path_matches, vec!["src/**/*.rs".to_owned()]);
    assert_eq!(
        declared.gates.path_not_matches,
        vec!["**/generated/**".to_owned()]
    );
    assert_eq!(declared.gates.file_contains, vec!["call".to_owned()]);
    assert_eq!(declared.gates.file_not_contains, vec!["skip".to_owned()]);
    assert_eq!(declared.timeout, Some(1500));
}

#[test]
fn options_reach_the_guest_as_json() {
    let (mut runtime, slot) = runtime_for("metadata");

    runtime
        .configure(slot, r#"{"allow":["a.rs"]}"#)
        .expect("the guest accepts what it can parse");
}

#[test]
fn a_guest_that_refuses_its_options_fails_the_call_with_its_own_message() {
    // A refusal is not a trap. A rule handed options it cannot use must be able to say so
    // in a message a user can act on — "expected an object" beats "wasm trap", which names
    // nothing the user wrote.
    let (mut runtime, slot) = runtime_for("metadata");

    let error = runtime
        .configure(slot, "[]")
        .expect_err("an array is not an options object");

    // The variant, not only the message. `Guest`'s `Display` is `"rule failed: {message}"`,
    // which also contains "expected an object" — so a message-only assertion passes whether
    // `configure` maps a refusal onto `Misconfigured` or reuses `Guest`, and proves nothing
    // about the distinction this variant exists for. Asserting the variant is what a mutation
    // from one to the other actually breaks.
    assert!(
        matches!(error, WasmError::Misconfigured { .. }),
        "a refusal must be its own variant, not a trap: {error:?}"
    );
    assert!(
        format!("{error}").contains("expected an object"),
        "the guest's own message should survive: {error}"
    );
}

#[test]
fn configuring_nothing_is_not_an_error() {
    // The bare-reference shape: a rule named with no options. `configure` is still called,
    // with `null`, so a guest has exactly one code path rather than two.
    let (mut runtime, slot) = runtime_for("metadata");

    runtime.configure(slot, "null").expect("null is no options");
}
