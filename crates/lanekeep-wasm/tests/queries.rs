//! `check-context`'s two scoped-query methods, driven by a real component.
//!
//! The host here is the host: `lanekeep_wasm::host` over a real [`NodeArena`], a real parse
//! and a real `lanekeep-query` compilation. What is under test is that a rule running as a
//! WebAssembly component gets the same matches, in the same order, at the same positions, as
//! the identical rule running under QuickJS — every assertion below restates one
//! `crates/lanekeep-js/src/host.rs` already makes, against the component boundary.
//!
//! # What a query answers when there is no grammar
//!
//! It cannot be asked. `lanekeep-js` installs `querySubtree` and `closestAncestor` only when a
//! language was attached, so a rule calling one without a grammar reaches a function that is
//! genuinely absent. WIT has no absent export — every method the world declares exists on
//! every component — so the same absence cannot cross, and this crate removes the state
//! instead: [`CheckContext::new`] takes the grammar, so a context that cannot compile a query
//! cannot be built. `src/host.rs`'s module header carries the reasoning and what the
//! alternatives cost.
//!
//! The consequence is what `an_invalid_query_is_reported_to_the_rule` asserts. The world says
//! the error case of `result<list<match>, string>` is "the query compiler's message", and with
//! no second kind of failure to carry, that stays true — so the test asserts the message *is*
//! the compiler's, which is the assertion a host overloading that channel would fail.

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
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, HostState, Report};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const QUERIES: &[u8] = include_bytes!("fixtures/queries.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

/// Two functions, the second holding two declarations the first does not.
///
/// The same source `lanekeep-js`'s `a_subtree_query_finds_only_what_is_inside` uses, so the
/// two suites are asking the same question of the same tree.
const TWO_FUNCTIONS: &str =
    "function a() { const x = 1; }\nfunction b() { const y = 2; const z = 3; }\n";

/// One function inside another, which is what makes "nearest" and "outermost" different
/// answers.
const NESTED: &str = "function outer() { function inner() { const x = 1; } }\n";

/// One declaration, for the probes that only need something to query.
const ONE_DECLARATION: &str = "const alpha = 1;\n";

/// Parse a source, lend a context over it to the fixture, and call one probe.
///
/// Returns the store and the context handle rather than the reports, so a test that needs to
/// look at the context again — at how many queries it compiled, say — can.
fn run(source: &str, name: &str) -> (Store<HostState>, Resource<CheckContext>) {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, QUERIES).expect("the fixture is a valid component");
    let mut linker = Linker::new(&engine);
    Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .expect("the real host satisfies every import the world declares");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parses");

    let mut store = Store::new(&engine, HostState::new());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an unreachable deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX);
    let context = store
        .data_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            FILE,
            Arc::new(TypeScript),
        ))
        .expect("the resource table accepts a context");

    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");
    let captures = vec![types::MatchEntry {
        name: name.to_owned(),
        node: NodeArena::ROOT,
    }];

    // Every probe here returns normally. Nothing on this surface traps: a query that does not
    // compile, a handle that resolves to nothing and an ancestor walk that finds nothing all
    // answer through the world's own channels, and a trap would crash a rule on a file it
    // could have skipped.
    rule.call_check(&mut store, Resource::new_borrow(context.rep()), &captures)
        .expect("check returns without trapping");

    (store, context)
}

/// Run one probe and collect what it reported.
fn probe(source: &str, name: &str) -> Vec<Report> {
    let (mut store, context) = run(source, name);
    store
        .data_mut()
        .check_context_mut(&context)
        .expect("the context outlives the call that borrowed it")
        .take_reports()
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
fn a_subtree_query_finds_only_what_is_inside() {
    // The point of scoping: a rule that matched a function and wants to look inside it should
    // not have to filter the whole file's matches by position. The whole-file count is in the
    // message beside the scoped one, because a scoped result that happened to be complete
    // would otherwise be indistinguishable from a scope that was ignored.
    assert_eq!(
        rendered(&probe(TWO_FUNCTIONS, "inside")),
        ["1:1 functions=2 scoped=y,z whole-file=3"]
    );
}

#[test]
fn a_subtree_query_with_no_matches_is_an_empty_list() {
    // So a guest's `for m in ctx.query_subtree(...)` is a no-op rather than an error, which is
    // the same shape `lanekeep-js` guarantees with an empty array.
    assert_eq!(
        rendered(&probe(ONE_DECLARATION, "no-matches")),
        ["1:1 ok len=0"]
    );
}

#[test]
fn every_capture_crosses_under_the_name_the_query_gave_it() {
    // `match` is a `list<match-entry>` rather than a record because capture names are the
    // rule's own and are not known to the world in advance. What has to survive the crossing
    // is the pairing: each name bound to the node the query bound it to, as a handle every
    // other method accepts.
    //
    // The position appears twice on purpose. `line=` and `column=` are what the guest read
    // through those methods; the `1:15` prefix is what the host derived from the same node
    // when the guest reported at it. One-based, line and column both — a zero-based reading
    // would put every component rule's violations off by one from the identical TypeScript
    // rule's, which looks like a rule bug and is not one.
    assert_eq!(
        rendered(&probe(ONE_DECLARATION, "captures")),
        ["1:15 name=alpha value=1 line=Some(1) column=Some(15)"]
    );
}

#[test]
fn an_invalid_query_is_reported_to_the_rule() {
    // Through the `result`'s error case, not as a trap. A rule can match on it and skip the
    // file; a trap would take the run down over a query string the rule may not even use on
    // every file.
    let reports = probe(ONE_DECLARATION, "invalid");
    let messages: Vec<&str> = reports
        .iter()
        .filter_map(|report| report.message.as_deref())
        .collect();
    let [subtree, ancestor] = messages.as_slice() else {
        panic!("both methods report: {messages:?}");
    };

    // The world says this channel carries "the query compiler's message", and because the
    // grammar is not optional there is no second kind of failure to put on it. This is what
    // that costs a host that ever overloaded it: the assertion is that the message *is*
    // `lanekeep-query`'s rendering, caret diagram and all, rather than merely non-empty.
    for message in [subtree, ancestor] {
        assert!(
            message.contains("query error: the query is not valid s-expression syntax"),
            "the compiler's own first line: {message}"
        );
        assert!(
            message.contains("--> query:1:"),
            "and its position within the query source: {message}"
        );
        assert!(
            message.contains('^'),
            "and the caret pointing at the offending token: {message}"
        );
    }

    assert_eq!(
        subtree.strip_prefix("query-subtree: "),
        ancestor.strip_prefix("closest-ancestor: "),
        "both methods compile through one cache, so they report one failure identically"
    );
}

#[test]
fn closest_ancestor_finds_the_nearest_one() {
    // Nearest, not outermost — the whole reason a rule walks upward. `outer` encloses `inner`
    // and matches the same pattern, so an implementation that walked from the root down, or
    // that kept looking after it found one, answers `outer` here.
    assert_eq!(
        rendered(&probe(NESTED, "nearest")),
        ["1:1 nearest=inner captures=2"]
    );
}

#[test]
fn closest_ancestor_is_none_when_nothing_matches() {
    // `option<match>` rather than an empty list, and the distinction is categorical rather
    // than conventional: a match that bound no captures is a real match, and a rule that read
    // an empty capture list as "nothing matched" would be right by accident until a query
    // bound nothing. The probe renders the two differently, so `some with 0 captures` fails
    // this rather than passing it.
    assert_eq!(rendered(&probe(ONE_DECLARATION, "nothing")), ["1:1 none"]);
}

#[test]
fn closest_ancestor_does_not_match_the_node_itself_from_inside() {
    // "Matches at" rather than "matches within". Every ancestor of `x` contains a number and
    // none of them is one, so `(number)` must find nothing however far up it walks — where an
    // implementation asking "does this query match anywhere below" would stop at the first
    // ancestor and make the innermost enclosing node the answer to every upward walk.
    //
    // Paired with a query that does match from the same node in the same tree, because a host
    // that answered `none` to everything would pass the first half on its own.
    assert_eq!(
        rendered(&probe(NESTED, "from-inside")),
        ["1:1 within=none at={ const x = 1; }"]
    );
}

#[test]
fn a_query_source_is_compiled_once_per_file() {
    // §6.2's performance property. A rule calling `querySubtree` inside its handler calls it
    // once per match with the same string every time, so compiling per call would make the
    // second-cheapest operation in the host the most expensive one.
    //
    // The probe makes six calls over two sources: three `query-subtree` and one
    // `closest-ancestor` with one query, and two more with a query that does not compile.
    let (mut store, context) = run(ONE_DECLARATION, "cache");
    let context = store
        .data_mut()
        .check_context_mut(&context)
        .expect("the context outlives the call that borrowed it");

    assert_eq!(
        rendered(&context.take_reports()),
        ["1:1 six calls over two sources"],
        "the probe made every call it was supposed to"
    );
    assert_eq!(
        context.queries_compiled(),
        2,
        "one compilation per distinct source, and the failure is cached too — a cache that \
         only remembered successes would recompile the malformed query on every match, which \
         is the one case where the work is certainly wasted"
    );
}

#[test]
fn a_query_at_an_unresolvable_handle_is_empty_rather_than_a_trap() {
    // The same posture the navigation methods take. Rule code is arbitrary and will pass stale
    // or invented numbers; trapping here would abort the run over a mistyped variable.
    //
    // `ok none` rather than `ok some with 0 captures` for the ancestor walk, for the reason
    // `closest_ancestor_is_none_when_nothing_matches` gives.
    assert_eq!(
        rendered(&probe(ONE_DECLARATION, "unresolvable")),
        ["1:1 subtree=ok len=0 ancestor=ok none"]
    );
}
