//! `check-context`'s binding resolution, driven by a real component.
//!
//! The host here is the host: `lanekeep_wasm::host` over a real [`NodeArena`], a real parse
//! and `lanekeep_lang_js`'s own resolver. What is under test is that a rule running as a
//! WebAssembly component resolves an identifier to the same binding the identical rule
//! resolves it to under QuickJS — every assertion below restates one
//! `crates/lanekeep-js/src/host.rs` already makes, against the component boundary.
//!
//! That correspondence is the point. A rule ported from TypeScript to a component must not
//! change its mind about whether `ms` is `makeStyles` from `@rneui/themed`, and both engines
//! reaching the same [`NodeArena`] only removes the question if they also agree about how to
//! read what it returns.
//!
//! # Not finding a binding is an answer, not an error
//!
//! Unlike the methods `tests/navigation.rs` covers, nothing here traps on an input it cannot
//! do anything with. A handle no arena issued, a name nothing declares, and a language whose
//! `Language::resolver` returns `None` all produce `false` and `none` — which is what
//! `lanekeep-js` has always answered, and is why the pair of tests over the `all` probe below
//! runs the same guest code over the same source twice, once with a resolver and once without.
//! A single run of it could not tell "the host answered no" from "the host answers no to
//! everything".

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` is listed because only it fires: nothing here panics
// directly, and an unfulfilled `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_lang_js::binding::JsBindingResolver;
use lanekeep_nodes::{Handle, NodeArena};
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, HostState, Report};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const BINDINGS: &[u8] = include_bytes!("fixtures/bindings.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

/// Whether the context under test has a resolver attached.
///
/// A named type rather than a bare `bool` at four call sites, because `probe(source, "all",
/// Some("a"), false)` says nothing about which `false` that is.
#[derive(Clone, Copy)]
enum Resolution {
    /// The language resolves identifiers, as TypeScript does.
    Resolved,
    /// The language has no resolver, as a language lanekeep parses but does not analyze.
    Unresolved,
}

/// Intern the last identifier reading `name`, the way a query capture arrives.
///
/// The same search `lanekeep-js`'s `handle_of` does, so the two suites are asking about the
/// same node of the same tree. Last rather than first because every source below writes the
/// declaration before the use, and the use is what a rule matches on.
fn intern_last_identifier(arena: &mut NodeArena, name: &str) -> Handle {
    let source = arena.source().to_owned();

    let path = {
        let mut best: Option<tree_sitter::Node<'_>> = None;
        let mut stack = vec![arena.tree().root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "identifier"
                && source.get(node.byte_range()) == Some(name)
                && best.is_none_or(|found| node.start_byte() > found.start_byte())
            {
                best = Some(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        arena
            .path_of(best.expect("the source contains the identifier under test"))
            .expect("the node belongs to this tree")
    };

    arena.intern_path(path).expect("the path interns")
}

/// Parse a source, lend a context over it to the fixture, and call one probe.
///
/// `target` is the identifier the probe asks about; it arrives as a capture named `target`,
/// interned by the host exactly as the engine will intern a query's captures.
fn probe(source: &str, name: &str, target: Option<&str>, resolution: Resolution) -> Vec<Report> {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, BINDINGS).expect("the fixture is a valid component");
    let mut linker = Linker::new(&engine);
    Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .expect("the real host satisfies every import the world declares");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parses");

    let mut context = CheckContext::new(
        NodeArena::new(tree, source.to_owned()),
        FILE,
        Arc::new(TypeScript),
    );
    if let Resolution::Resolved = resolution {
        context = context.with_resolver(Arc::new(JsBindingResolver));
    }

    let mut captures = vec![types::MatchEntry {
        name: name.to_owned(),
        node: NodeArena::ROOT,
    }];
    if let Some(target) = target {
        captures.push(types::MatchEntry {
            name: "target".to_owned(),
            node: intern_last_identifier(context.arena_mut(), target),
        });
    }

    let mut store = Store::new(&engine, HostState::new());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an out-of-reach deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX / 2);
    let context = store
        .data_mut()
        .push_check_context(context)
        .expect("the resource table accepts a context");

    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");
    // Rule 0: this fixture hosts one rule, so `rules()` has one entry and every other export
    // answers to index zero.
    rule.call_check(
        &mut store,
        0,
        Resource::new_borrow(context.rep()),
        &captures,
    )
    .expect("check returns without trapping")
    .expect("the rule does not report a failure");

    store
        .data_mut()
        .check_context_mut(&context)
        .expect("the context outlives the call that borrowed it")
        .take_reports()
}

/// The common case: a resolver attached and one identifier under test.
fn resolved(source: &str, name: &str, target: &str) -> Vec<Report> {
    probe(source, name, Some(target), Resolution::Resolved)
}

/// Render reports as `line:column message`, so an assertion reads as what a reader would see
/// and a failure names the position rather than dumping a struct.
fn rendered(reports: &[Report]) -> Vec<String> {
    reports
        .iter()
        .map(|report| {
            format!(
                "{}:{} {}",
                report.line,
                report.column,
                report.message.as_deref().unwrap_or("<no message>")
            )
        })
        .collect()
}

#[test]
fn resolves_an_import_through_its_alias() {
    // The case §6.4 exists for. A rule looking for `makeStyles` has to find `ms`.
    assert_eq!(
        rendered(&resolved(
            "import { makeStyles as ms } from '@rneui/themed';\nms();",
            "alias",
            "ms",
        )),
        ["2:1 exact=true wrong-module=false wrong-name=false"]
    );
}

#[test]
fn a_local_declaration_does_not_resolve_to_the_import_it_shadows() {
    // The false positive this prevents: a rule keyed on the name firing on a local that has
    // nothing to do with the import.
    assert_eq!(
        rendered(&resolved(
            "import { makeStyles } from '@rneui/themed';\n\
             function f() { const makeStyles = () => {}; return makeStyles(); }",
            "shadow",
            "makeStyles",
        )),
        ["2:52 resolves-to-import=false kind=const shadowed=true"]
    );
}

#[test]
fn omitting_the_name_matches_any_export_of_the_module() {
    // `default` is asked for as well as a name the module does not export, because both are
    // answers a host that dropped the `option<string>` and matched on the module alone would
    // get wrong in the same direction.
    assert_eq!(
        rendered(&resolved("import { a } from 'm';\na();", "any-export", "a")),
        ["2:1 omitted=true named=true wrong-name=false default=false"]
    );
}

#[test]
fn matches_a_module_by_glob() {
    assert_eq!(
        rendered(&resolved(
            "import { a } from '@scope/pkg';\na();",
            "glob",
            "a",
        )),
        ["2:1 scope-star=true star-pkg=true exact=true other-scope=false"]
    );
}

#[test]
fn reports_binding_kinds() {
    for (source, name, expected) in [
        ("import { a } from 'm';\na();", "a", "import"),
        ("const b = 1;\nb;", "b", "const"),
        ("let c = 1;\nc;", "c", "let"),
        ("function d() {}\nd();", "d", "function"),
        ("class E {}\nnew E();", "E", "class"),
        ("function f(p) { return p; }", "p", "param"),
    ] {
        let reports = resolved(source, "kind", name);
        assert_eq!(
            reports.first().and_then(|report| report.message.as_deref()),
            Some(expected),
            "for {name} in {source}"
        );
    }
}

#[test]
fn an_undeclared_name_has_no_binding_kind() {
    // `option<binding-kind>` rather than a `binding-kind` with an "unknown" case: a rule that
    // asks about a global gets nothing, not a kind it would have to know to ignore.
    assert_eq!(
        rendered(&resolved("globalThing();", "kind", "globalThing")),
        ["1:1 none"]
    );
}

#[test]
fn with_a_resolver_the_four_answers_are_the_bindings_own() {
    // The control for the test below. Same guest code, same source, same handle — so the pair
    // shows that "nothing resolves" is what the *absent resolver* produces, rather than what
    // this probe produces whatever the host does.
    assert_eq!(
        rendered(&resolved("import { a } from 'm';\na();", "all", "a")),
        ["1:1 resolves-to-import=true imported-from=true shadowed=false kind=import"]
    );
}

#[test]
fn without_a_resolver_nothing_resolves_rather_than_trapping() {
    // A language with no resolver makes rules see "nothing resolves", which is a truthful
    // answer — the same one `lanekeep-js` gives, where the functions are installed regardless
    // and close over an `Option`. The component model has no way to make a method absent for
    // one file and present for another anyway: a world declares all four or none.
    assert_eq!(
        rendered(&probe(
            "import { a } from 'm';\na();",
            "all",
            Some("a"),
            Resolution::Unresolved,
        )),
        ["1:1 resolves-to-import=false imported-from=false shadowed=false kind=none"]
    );
}

#[test]
fn an_unresolvable_handle_resolves_to_nothing_rather_than_trapping() {
    // Rule code is arbitrary and will pass stale or invented numbers. Trapping here would
    // abort the run over a mistyped variable in a rule — and unlike navigation, there is not
    // even a distinction to preserve: "that handle is not live" and "that name does not
    // resolve" are the same answer to a rule.
    assert_eq!(
        rendered(&probe(
            "import { a } from 'm';\na();",
            "unresolvable",
            None,
            Resolution::Resolved,
        )),
        ["1:1 resolves-to-import=false imported-from=false shadowed=false kind=none"]
    );
}
