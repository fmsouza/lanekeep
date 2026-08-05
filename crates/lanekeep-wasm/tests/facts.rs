//! `check-context`'s `emit-fact`, driven by a real component.
//!
//! The host here is the host: `lanekeep_wasm::host` over a real [`CheckContext`]. What is under
//! test is the one place the host API's *contract* changes rather than its calling convention.
//! Under QuickJS a fact is a JavaScript value and `JSON.stringify` makes "well-formed JSON
//! object" true by construction; a component hands over a `string` produced by whatever
//! serializer its language has, and the host has to find out. `src/facts.rs` carries the
//! reasoning and pins the three answers down without a runtime; this file shows they survive
//! the boundary, that a refusal records nothing, and that what is recorded is what the guest
//! sent.
//!
//! # Every payload comes from this file
//!
//! The guest emits what the host passes it. So the parse message a refusal carries can be
//! compared against what `serde_json` says about the *same bytes* — see
//! [`a_payload_that_is_not_json_is_rejected_with_the_parser_message`] — rather than against a
//! substring, which would pass for a host that invented its own wording. It also means one
//! component covers every case in the suite.
//!
//! # The phase split is a property of the world, and it is checked as one
//!
//! `emit-fact` belongs to `check-context` and must not exist on `reduce-context`; a per-file
//! rule that could read the corpus would make a file's result depend on files other than
//! itself, and caching that result against its own content would be unsound.
//! [`the_world_puts_emit_fact_on_the_check_context_and_nowhere_else`] asserts that against
//! `wit/world.wit`, which is the declared source of truth and whose bytes are a cache-key input.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Two lints rather than one, and both are listed because both actually fire: an
// unfulfilled `expect` attribute is itself an error, so a speculative third would fail the
// gate. `expect_used` fires throughout; `panic` fires once, in [`resource_body`], which cannot
// name the resource it could not find through `expect`.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, Fact, HostState};
use wasmtime::Store;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const FACTS: &[u8] = include_bytes!("fixtures/facts.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

/// The file under check. Nothing here queries it; a context has to have parsed something.
const SOURCE: &str = "const alpha = 1;\n";

/// One instantiated component, one store, and one context lent across every call.
///
/// The context survives between calls deliberately: the fact list is a property of a
/// `check-context`, and a harness that rebuilt one per call could not observe that a second
/// invocation appends to what the first emitted.
struct Harness {
    store: Store<HostState>,
    rule: Rule,
    context: Resource<CheckContext>,
}

impl Harness {
    fn new() -> Self {
        let engine = engine().expect("the shipped wasmtime configuration builds an engine");
        let component = Component::new(&engine, FACTS).expect("the fixture is a valid component");
        let mut linker = Linker::new(&engine);
        Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .expect("the real host satisfies every import the world declares");

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(SOURCE, None).expect("parses");

        let context = CheckContext::new(
            NodeArena::new(tree, SOURCE.to_owned()),
            FILE,
            Arc::new(TypeScript),
        );

        let mut store = Store::new(&engine, HostState::new());
        let context = store
            .data_mut()
            .push_check_context(context)
            .expect("the resource table accepts a context");
        let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

        Self {
            store,
            rule,
            context,
        }
    }

    /// Invoke one probe and collect the messages it reported.
    ///
    /// The call is expected to return. A host that trapped on a fact it did not like would fail
    /// here rather than on any assertion about the rendering, which is the right place: the
    /// world declares `result<_, fact-error>`, so a refusal is a value.
    fn probe(&mut self, probe: &str, args: &[&str]) -> Vec<String> {
        // The probe name first, then one entry per argument. Handles are all the root's, which
        // is zero — nothing on this surface reads them, and zero is the handle a truthiness
        // test would discard.
        let captures: Vec<types::MatchEntry> = std::iter::once(probe)
            .chain(args.iter().copied())
            .map(|name| types::MatchEntry {
                name: name.to_owned(),
                node: NodeArena::ROOT,
            })
            .collect();

        self.rule
            .call_check(
                &mut self.store,
                Resource::new_borrow(self.context.rep()),
                &captures,
            )
            .expect("check returns without trapping");

        self.store
            .data_mut()
            .check_context_mut(&self.context)
            .expect("the context outlives the call that borrowed it")
            .take_reports()
            .into_iter()
            .map(|report| report.message.unwrap_or_else(|| "<no message>".to_owned()))
            .collect()
    }

    /// What the context would hand the engine, emptying it as the engine's call does.
    fn facts(&mut self) -> Vec<Fact> {
        self.store
            .data_mut()
            .check_context_mut(&self.context)
            .expect("the context outlives the call that borrowed it")
            .take_facts()
    }

    /// The recorded facts as `(kind, data)` couples, for the assertions that read both.
    fn recorded(&mut self) -> Vec<(String, String)> {
        self.facts()
            .into_iter()
            .map(|fact| (fact.kind, fact.data))
            .collect()
    }
}

/// What `serde_json` says about a payload, which is what the host must say about it too.
fn parse_message(data: &str) -> String {
    serde_json::from_str::<serde_json::Value>(data)
        .expect_err("the payload was chosen because it does not parse")
        .to_string()
}

#[test]
fn a_well_formed_fact_is_recorded_with_its_kind_and_payload() {
    let mut harness = Harness::new();

    assert_eq!(
        harness.probe("emit", &["export", r#"{"symbol":"parse"}"#]),
        ["emit=ok"]
    );
    assert_eq!(
        harness.recorded(),
        [("export".to_owned(), r#"{"symbol":"parse"}"#.to_owned())]
    );
}

#[test]
fn facts_are_kept_in_emission_order() {
    // Within a file, the order a rule emitted in is the only order it can have meant — and it
    // is what `lanekeep_core::Fact::sequence` is assigned from. The kinds repeat on purpose: a
    // host that grouped or deduplicated by kind would produce `a, a, b` and pass an assertion
    // written on the set.
    let mut harness = Harness::new();

    assert_eq!(
        harness.probe(
            "pairs",
            &["a", r#"{"n":1}"#, "b", r#"{"n":2}"#, "a", r#"{"n":3}"#],
        ),
        ["ok", "ok", "ok"]
    );
    assert_eq!(
        harness.recorded(),
        [
            ("a".to_owned(), r#"{"n":1}"#.to_owned()),
            ("b".to_owned(), r#"{"n":2}"#.to_owned()),
            ("a".to_owned(), r#"{"n":3}"#.to_owned()),
        ]
    );
}

#[test]
fn a_fact_with_an_empty_kind_is_rejected_and_not_recorded() {
    // `kind` is what `facts(kind)` selects on, so a fact without one can never be read back.
    // Accepting it would leave a rule looking correct right up until the reduce phase found
    // nothing — which is why the payload here is perfectly valid.
    let mut harness = Harness::new();

    assert_eq!(
        harness.probe("emit", &["", r#"{"n":1}"#]),
        ["emit=empty-kind"]
    );
    assert!(
        harness.recorded().is_empty(),
        "a refused fact is not recorded"
    );
}

#[test]
fn a_payload_that_is_not_json_is_rejected_with_the_parser_message() {
    // An unterminated object: the case with no QuickJS predecessor at all, because
    // `JSON.stringify` could not have produced it.
    let malformed = r#"{"symbol":"parse""#;
    let mut harness = Harness::new();

    assert_eq!(
        harness.probe("emit", &["export", malformed]),
        [format!("emit=invalid-json({})", parse_message(malformed))],
        "the case carries the parser's own message, not one the host wrote"
    );
    assert!(harness.recorded().is_empty());
}

#[test]
fn a_payload_that_is_json_and_not_an_object_is_rejected() {
    // Every other thing a JSON document can be. `null` is the one worth naming: it parses, it
    // is a perfectly good document, and a fact made of it carries nothing to read back.
    let mut harness = Harness::new();

    let payloads = ["[1,2,3]", "\"export\"", "42", "null", "true"];
    let couples: Vec<&str> = payloads.iter().flat_map(|data| ["export", data]).collect();

    assert_eq!(harness.probe("pairs", &couples), ["not-an-object"; 5]);
    assert!(harness.recorded().is_empty());
}

#[test]
fn a_refused_fact_does_not_end_the_invocation() {
    // The whole reason the refusal is a `result` rather than a trap: the rule is still running
    // afterwards, and its later facts are still recorded. The two bad couples sit around the
    // good ones, so a host that unwound the instance would lose everything after the first.
    let mut harness = Harness::new();

    assert_eq!(
        harness.probe(
            "pairs",
            &[
                "export",
                "{oops",
                "export",
                r#"{"n":1}"#,
                "",
                "{}",
                "export",
                r#"{"n":2}"#,
            ],
        ),
        [
            format!("invalid-json({})", parse_message("{oops")),
            "ok".to_owned(),
            "empty-kind".to_owned(),
            "ok".to_owned(),
        ]
    );
    assert_eq!(
        harness.recorded(),
        [
            ("export".to_owned(), r#"{"n":1}"#.to_owned()),
            ("export".to_owned(), r#"{"n":2}"#.to_owned()),
        ],
        "the two accepted facts, in order, and neither refusal left a trace"
    );
}

#[test]
fn the_payload_is_recorded_byte_for_byte_as_the_guest_sent_it() {
    // Nothing is re-serialized, and this is what says so. The keys are out of alphabetical
    // order and the spacing is a serializer's own: a round trip through `serde_json::Value`
    // would sort them — its `Map` is a `BTreeMap` unless a feature flag says otherwise — and
    // would drop the whitespace, so a host that canonicalized would put this workspace's build
    // configuration into the bytes that reach the cache.
    let payload = "{ \"z\" : 1 , \"a\" : [ 2, 3 ] , \"m\" : null }";
    let mut harness = Harness::new();

    assert_eq!(harness.probe("emit", &["export", payload]), ["emit=ok"]);
    assert_eq!(
        harness.recorded(),
        [("export".to_owned(), payload.to_owned())]
    );
}

#[test]
fn no_file_is_merged_into_the_payload() {
    // A departure from `lanekeep-js`, stated as an assertion rather than only in a comment.
    // Its `merge_file` splices a `"file"` key into a fact's payload so a rule cannot
    // misattribute a cross-file violation by shadowing it — but the engine calls it at *reduce*
    // time, not at emit time, and the world removes the need for it here: `emitted-fact` gives
    // `file` its own field, which the host fills from the context. Merging at emit time would
    // put a key in the stored payload that the engine's reduce-time merge would then duplicate.
    //
    // The rule's own `"file"` therefore survives untouched, because inside `data` it is inert.
    let mut harness = Harness::new();
    let payload = r#"{"file":"lies.ts"}"#;

    harness.probe("emit", &["export", payload]);
    let recorded = harness.recorded();

    assert_eq!(recorded, [("export".to_owned(), payload.to_owned())]);
    assert!(
        !recorded[0].1.contains(FILE),
        "the context's own path is not spliced into the payload: {recorded:?}"
    );
}

#[test]
fn taking_the_facts_empties_the_context() {
    // For the reason `take_reports` empties: a context read twice must not emit a fact twice.
    // It matters more here, because a duplicated fact does not merely appear twice in output —
    // it changes what a counting or first-seen-wins `reduce` concludes about the corpus.
    let mut harness = Harness::new();

    harness.probe("emit", &["export", "{}"]);
    assert_eq!(harness.recorded().len(), 1);
    assert!(
        harness.recorded().is_empty(),
        "the second take sees nothing"
    );
}

#[test]
fn a_second_invocation_appends_to_what_the_first_emitted() {
    // One `check-context` per file, several `check` calls per file — one per query match. The
    // facts accumulate across them, because they are the file's and not the match's.
    let mut harness = Harness::new();

    harness.probe("emit", &["export", r#"{"n":1}"#]);
    harness.probe("emit", &["export", r#"{"n":2}"#]);

    assert_eq!(
        harness.recorded(),
        [
            ("export".to_owned(), r#"{"n":1}"#.to_owned()),
            ("export".to_owned(), r#"{"n":2}"#.to_owned()),
        ]
    );
}

#[test]
fn the_artifact_reaches_emit_fact_through_the_check_context() {
    // The boundary spelling, read off the built component rather than off this crate's
    // bindings. A component's embedded WIT is a *subset* of the world — it lists only what the
    // guest calls — so what this asserts is that the method the fixture reached is
    // `check-context`'s, and that nothing on `reduce-context` was reachable at all.
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, FACTS).expect("the fixture is a valid component");
    let ty = component.component_type();

    let (_, types) = ty
        .imports(&engine)
        .find(|(name, _)| *name == "lanekeep:host/types@0.1.0")
        .expect("the world's one instance import is there");
    let ComponentItem::ComponentInstance(instance) = types.ty else {
        panic!("the world's import is an instance");
    };

    let mut methods: Vec<&str> = instance
        .exports(&engine)
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("[method]"))
        .collect();
    methods.sort_unstable();

    assert_eq!(
        methods,
        vec![
            "[method]check-context.emit-fact",
            "[method]check-context.report",
            "[method]check-context.root",
        ],
        "exactly the three the fixture calls, and `emit-fact` is one of check-context's"
    );
}

/// The `resource <name> { ... }` block from `wit/world.wit`, with its comments removed.
///
/// Brace-matched rather than read to the next `}`, because the block contains none but a later
/// edit could. Comments are stripped because **every WIT comment is a doc comment, including
/// `//`** — so a note added to `reduce-context` explaining that it has no `emit-fact` would
/// otherwise fail the assertion below, which would be the check firing on the documentation
/// rather than on the declaration.
fn resource_body(source: &str, name: &str) -> String {
    let header = format!("resource {name} {{");
    let start = source
        .find(&header)
        .unwrap_or_else(|| panic!("`{name}` is declared in wit/world.wit"))
        + header.len();

    let mut depth = 1_usize;
    let mut end = start;
    for (offset, character) in source[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + offset;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(depth == 0, "`{name}`'s block is not closed");

    source[start..end]
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_world_puts_emit_fact_on_the_check_context_and_nowhere_else() {
    // The invariant that makes per-file cache entries mean anything, checked against the file
    // that declares it. A reduce phase that could emit facts could feed itself and there is no
    // second pass for the result to reach; a per-file rule that could read `facts` or `files`
    // would make a file's result depend on files other than itself.
    //
    // This is enforcement rather than documentation because the method sets are *disjoint*: a
    // wrong-phase call does not compile in any guest language, since the method is not on the
    // type the rule holds. The residual weakness the world names in full is that a per-file
    // rule can still *name* `reduce-context` and forge a handle — what it cannot do is obtain a
    // valid one, because the instance's table for the other phase's resource is empty during
    // the call and the forgery traps before the host is reached.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wit/world.wit"),
    )
    .expect("the world is where this crate says it is");

    let check = resource_body(&source, "check-context");
    let reduce = resource_body(&source, "reduce-context");

    assert!(
        check.contains("emit-fact:"),
        "the per-file phase declares emit-fact: {check}"
    );
    assert!(
        !reduce.contains("emit-fact"),
        "and the cross-file phase does not: {reduce}"
    );

    assert!(
        reduce.contains("facts:") && reduce.contains("files:"),
        "the cross-file phase declares facts and files: {reduce}"
    );
    assert!(
        !check.contains("facts:") && !check.contains("files:"),
        "and the per-file phase declares neither: {check}"
    );

    // Not vacuous: both blocks were found and both hold declarations. A `resource_body` that
    // returned an empty string would satisfy all four negations above on its own.
    assert!(
        check.contains("report:") && reduce.contains("report:"),
        "both blocks were read, and both declare the method they share"
    );
}
