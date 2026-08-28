//! The `metadata` and `configure` exports: what a component says about itself, and the
//! options it accepts.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests reaches neither the helpers below nor a \
              `tests/` integration crate."
)]

mod common;

use std::sync::Arc;

use common::{runtime_for, runtime_for_options};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::WasmError;
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::runtime::WasmRuntime;
use wasmtime::component::Resource;

#[test]
fn a_component_declares_its_own_identity() {
    let (mut runtime, slot) = runtime_for("metadata");

    let declared = runtime.metadata(slot).expect("the export answers");

    assert_eq!(declared.id, "fixture/metadata");
    assert_eq!(declared.languages, vec!["rust".to_owned()]);
    assert_eq!(declared.severity, "error");
    assert_eq!(
        declared.queries,
        vec![lanekeep_wasm::bindings::types::QueryFor {
            language: "rust".to_owned(),
            query: "(call_expression) @call".to_owned(),
        }]
    );
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
    // Through the only door there is: the options are given to the rule set, and the runtime
    // hands them to the instance it builds. The fixture refuses anything that is not an object
    // or `null`, so an acceptance here is the guest having read what was recorded.
    let (mut runtime, slot) = runtime_for_options("metadata", r#"{"allow":["a.rs"]}"#);

    runtime
        .rule(slot)
        .expect("the guest accepts what it can parse");
}

#[test]
fn a_guest_that_refuses_its_options_fails_the_call_with_its_own_message() {
    // A refusal is not a trap. A rule handed options it cannot use must be able to say so
    // in a message a user can act on — "expected an object" beats "wasm trap", which names
    // nothing the user wrote.
    let (mut runtime, slot) = runtime_for_options("metadata", "[]");

    let error = runtime
        .rule(slot)
        .err()
        .expect("an array is not an options object");

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

    runtime.rule(slot).expect("null is no options");
}

#[test]
fn nothing_reaches_a_check_without_having_been_configured() {
    // The ordering the world declares, as a property of this crate rather than of whoever
    // remembers to call something. `configure` runs between the instantiation and the instance
    // being handed out, so a guest that refuses its options stops the run at the first thing
    // that needs an instance — here a `check`, which is the call the ordering is written for.
    //
    // Written against `check` rather than against `rule` on purpose: `rule` is where the call
    // sits, and asserting there would pass against an implementation that configured lazily
    // somewhere else. This asserts what the world promises.
    let (mut runtime, slot) = runtime_for_options("metadata", "[]");

    let context = context(&mut runtime);
    let error = runtime
        .check(slot, &context, &Vec::new())
        .expect_err("a rule that refused its options must not be asked to check anything");

    assert!(
        matches!(error, WasmError::Misconfigured { .. }),
        "the refusal must survive to the caller unchanged: {error:?}"
    );
}

#[test]
fn a_component_naming_no_language_is_refused() {
    // `{"no-language":true}` makes the fixture's `metadata` return an empty `languages` list.
    // An empty list is not "every language" — it is *no file at all*, and silently: the rule
    // loads and reports nothing, which is indistinguishable from the code being clean. The host
    // refuses it at metadata time, before any file is checked, which is what this asserts.
    let (mut runtime, slot) = runtime_for_options("metadata", r#"{"no-language":true}"#);

    let error = runtime
        .metadata(slot)
        .expect_err("a component whose metadata names no language must be refused at load");

    assert!(
        matches!(error, WasmError::InvalidMetadata { .. }),
        "the refusal must be the named variant, not a trap: {error:?}"
    );
    assert!(
        format!("{error}").contains("fixture/metadata"),
        "the refusal must name the rule: {error}"
    );
}

#[test]
fn a_component_declaring_a_conjunctive_content_gate_is_refused() {
    // `{"bad-gate":true}` makes the fixture's `metadata` return a `file_contains` gate of two
    // substrings. A content gate is an *and* — every substring must be present — so a file
    // containing only one is rejected, and the rule reports nothing while looking healthy. The
    // host refuses it at metadata time.
    let (mut runtime, slot) = runtime_for_options("metadata", r#"{"bad-gate":true}"#);

    let error = runtime
        .metadata(slot)
        .expect_err("a content gate listing more than one substring must be refused at load");

    assert!(
        matches!(error, WasmError::InvalidMetadata { .. }),
        "the refusal must be the named variant, not a trap: {error:?}"
    );
    assert!(
        format!("{error}").contains("fixture/metadata"),
        "the refusal must name the rule: {error}"
    );
}

/// A check context over one trivial file, so `check` has something to be given.
fn context(runtime: &mut WasmRuntime) -> Resource<CheckContext> {
    let source = "const x = 1;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the grammar loads");
    let tree = parser.parse(source, None).expect("it parses");

    runtime
        .host_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            "src/a.ts",
            Arc::new(TypeScript),
        ))
        .expect("the resource table accepts a context")
}
