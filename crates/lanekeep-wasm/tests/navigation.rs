//! `check-context`'s navigation and reporting, driven by a real component.
//!
//! Unlike `tests/world_shape.rs`, the host here is the host: `lanekeep_wasm::host` over a
//! real `NodeArena` built from a real parse. What is under test is that a rule running as a
//! WebAssembly component sees the same tree, at the same positions, as the identical rule
//! running under QuickJS — every assertion below is the one
//! `crates/lanekeep-js/src/host.rs`'s test of the same name makes, restated against the
//! component boundary.
//!
//! That correspondence is the point rather than a convenience. Two engines that disagreed
//! about what `parent` or `text` means for one file would let a single run disagree with
//! itself, which is what sharing one `NodeArena` between them exists to prevent — and a
//! shared type only removes the question if both callers actually agree about how to call it.
//!
//! # The guest is a probe and the assertions live here
//!
//! `tests/fixtures/navigation/` walks the tree and encodes what it observed into each
//! report's message; this file asserts on the recorded reports. So each assertion below
//! covers two things at once: the message, which is what the guest saw through the boundary,
//! and the line and column, which the host derived independently from the node the guest
//! reported at.

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
const NAVIGATION: &[u8] = include_bytes!("fixtures/navigation.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

/// The two-statement source most probes run against.
///
/// The same string `lanekeep-js`'s `reads_text_and_position` uses, so the line and column
/// numbers asserted here are comparable to that test's by inspection.
const TWO_STATEMENTS: &str = "const x = 1;\nconst y = 2;";

/// One instantiated component, one store, and one context lent across every call.
///
/// The context outlives each call deliberately, and that is now the engine's shape rather than
/// an open question: `lanekeep-engine` builds **one context per file** and lends it to every
/// rule checking that file, so this harness mirrors production. A harness that rebuilt one per
/// call could not observe anything that accumulates, and the date-read flag is exactly that.
struct Run {
    store: Store<HostState>,
    rule: Rule,
    context: Resource<CheckContext>,
}

impl Run {
    /// Parse a source and lend a context over it to the fixture.
    ///
    /// `today` is what the context answers through the world's `today`, and `None` means the
    /// host supplied no date — the world's own declared answer rather than a missing one. It is
    /// always a literal and never a clock read: a test that asked the machine what day it was
    /// would pass today and fail at midnight, which is the failure this whole surface sits in
    /// tension with.
    fn new(source: &str, today: Option<&str>) -> Self {
        let engine = engine().expect("the shipped wasmtime configuration builds an engine");
        let component =
            Component::new(&engine, NAVIGATION).expect("the fixture is a valid component");
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
        if let Some(today) = today {
            context = context.with_today(today);
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

        Self {
            store,
            rule,
            context,
        }
    }

    /// Invoke one probe. An `Err` here is a trap.
    ///
    /// Separate from [`Run::probe`] because two tests need the error rather than the reports,
    /// and a helper that unwrapped would make them unwritable.
    fn call(&mut self, name: &str) -> wasmtime::Result<()> {
        let captures = vec![types::MatchEntry {
            name: name.to_owned(),
            node: NodeArena::ROOT,
        }];
        match self.rule.call_check(
            &mut self.store,
            0,
            Resource::new_borrow(self.context.rep()),
            &captures,
        )? {
            Ok(()) => Ok(()),
            // A Rust guest has no stack to hand back and reports a failure by trapping, so
            // this probe never takes the world's graceful channel. Folding one into an error
            // rather than ignoring it keeps "an `Err` here is a trap" true of the signature
            // without silently passing a failure off as a success.
            Err(failure) => Err(wasmtime::Error::msg(format!(
                "the guest returned a failure rather than trapping: {failure:?}"
            ))),
        }
    }

    /// Invoke one probe and collect what it reported.
    fn probe(&mut self, name: &str) -> Vec<Report> {
        self.call(name).expect("check returns without trapping");
        self.take()
    }

    /// Everything the context has recorded since it was last asked.
    fn take(&mut self) -> Vec<Report> {
        self.context_mut().take_reports()
    }

    /// Whether anything has read the date through this context.
    ///
    /// The boolean `lanekeep-engine` turns into a dated cache key, asked of the same accessor
    /// the engine asks of `lanekeep-js`.
    fn date_was_read(&mut self) -> bool {
        self.context_mut().date_was_read()
    }

    fn context_mut(&mut self) -> &mut CheckContext {
        self.store
            .data_mut()
            .check_context_mut(&self.context)
            .expect("the context outlives the call that borrowed it")
    }
}

/// Build a context with no date and run one probe, collecting what it reported.
///
/// The date is the one surface most of these probes never touch, so `None` is the default a
/// test opts out of rather than a value every test has to state.
fn probe(source: &str, name: &str) -> Vec<Report> {
    Run::new(source, None).probe(name)
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
fn exposes_the_file_path_and_text() {
    assert_eq!(
        rendered(&probe("const x = 1;", "file")),
        ["1:1 src/example.ts | const x = 1;"],
        "the guest read both whole-file accessors through the borrowed context"
    );
}

#[test]
fn navigates_from_the_root() {
    assert_eq!(
        rendered(&probe(TWO_STATEMENTS, "navigate")),
        [
            "1:1 program",
            "1:1 2 named children",
            "1:1 lexical_declaration",
            "2:1 const y = 2;",
            // The load-bearing line. `lanekeep-js`'s `reads_text_and_position` asserts 2 and
            // 1 for this node, and so does `lanekeep-query` when it builds a location, so a
            // zero-based reading here would put every component rule's violations one line
            // and one column off what the identical TypeScript rule reports.
            "2:1 line=Some(2) column=Some(1)",
        ]
    );
}

#[test]
fn walks_up_and_back_down() {
    assert_eq!(
        rendered(&probe("const x = 1;", "walk")),
        [
            "1:1 parent-of-inner-is-declaration=true parent-of-declaration-is-root=true \
             root-has-parent=false same-node-same-handle=true",
            // The root's handle is zero, and `parent` returning `option<node>` is the whole
            // reason: a host that collapsed absence and zero together would answer `None`
            // here, and every top-level item in every file would look parentless.
            "1:1 root=0 parent-of-declaration=Some(0)",
        ]
    );
}

#[test]
fn ancestors_end_at_the_root() {
    assert_eq!(
        rendered(&probe("function f() { return 1; }", "ancestors")),
        [
            "1:16 len=3 first-is-body=true last-is-root=true contains-function=true \
             root-ancestors=0"
        ]
    );
}

#[test]
fn named_children_omits_anonymous_tokens() {
    assert_eq!(
        rendered(&probe("const x = 1;", "named")),
        ["1:1 all=3 named=1 first-token-is-named=false every-named-child-is-named=true"]
    );
}

#[test]
fn an_unresolvable_handle_returns_nothing_rather_than_trapping() {
    // Rule code is arbitrary and will pass stale or invented numbers. Trapping here would
    // abort the run over a mistyped variable in a rule.
    let reports = probe("const x = 1;", "unresolvable");

    assert_eq!(
        rendered(&reports),
        [
            "1:1 kind=None text=None line=None column=None parent=None is-named=false \
             children=0 named-children=0 ancestors=0 loc=false"
        ]
    );
    assert_eq!(
        reports.len(),
        1,
        "the guest also reported *at* the unresolvable handle, and that one must be dropped \
         rather than recorded at a position a reader would go and look at"
    );
}

#[test]
fn loc_carries_the_file_and_the_same_position_a_report_would() {
    assert_eq!(
        rendered(&probe(TWO_STATEMENTS, "loc")),
        ["2:1 src/example.ts:2:1"]
    );
}

#[test]
fn structure_fingerprint_reports_the_hosts_own_fold() {
    // The guest reports what it saw through the boundary; the assertion compares it against
    // a fresh arena's answer computed here, host-side. Two parses of one source must fold
    // identically, so a mismatch means the WIT host wired the arena's method wrong — the
    // one thing a stub host (world_shape.rs) cannot see. The dead handle's `none` is
    // asserted too: nothing rather than a fabricated shape.
    let reports = probe(TWO_STATEMENTS, "fingerprint");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(TWO_STATEMENTS, None).expect("parses");
    let expected = NodeArena::new(tree, TWO_STATEMENTS.to_owned())
        .structure_fingerprint(NodeArena::ROOT)
        .expect("the root resolves");

    assert_eq!(
        rendered(&reports),
        [
            format!("1:1 fp={}:{}", expected.hash, expected.nodes),
            "1:1 dead=true".to_owned(),
        ]
    );
}

#[test]
fn a_fix_is_a_byte_range_taken_from_the_node_it_names() {
    // A rule already has the node it matched; offsets it computed itself are offsets it can
    // get wrong, which is the one mistake that would let a fix corrupt a file.
    let reports = probe(TWO_STATEMENTS, "fix");

    assert_eq!(
        rendered(&reports),
        [
            "2:1 replace it",
            "1:1 a suggestion",
            "1:1 a fix at a handle that does not resolve",
        ]
    );

    let fixes: Vec<Option<(usize, usize, &str, bool)>> = reports
        .iter()
        .map(|report| {
            report
                .fix
                .as_ref()
                .map(|fix| (fix.start, fix.end, fix.replacement.as_str(), fix.safe))
        })
        .collect();
    assert_eq!(
        fixes,
        [
            Some((13, 25, "const y = 3;", true)),
            Some((0, 12, "const x = 2;", false)),
            // Dropped rather than guessed at: a fix at the wrong offsets rewrites the wrong
            // code. The report it came with survives, because what the rule found is still
            // true.
            None,
        ]
    );
}

#[test]
fn a_fix_that_does_not_say_whether_it_is_safe_is_a_suggestion() {
    // The reason `fix.safe` is `option<bool>` rather than `bool`. `--fix` applies the safe
    // ones, so the two mistakes are not symmetric: a suggestion that should have been safe
    // costs a manual edit, and a fix that should have been a suggestion rewrites someone's
    // code. A `bool` cannot carry "did not say", so every authoring crate would have to pick a
    // value on its author's behalf, and one picking `true` would make fixes auto-applicable
    // with nothing here able to notice. This asserts the host owns that answer.
    let reports = probe("const x = 1;", "unsaid-fix");
    let fix = reports
        .first()
        .and_then(|report| report.fix.as_ref())
        .expect("the fix survives, since the node it names resolves");

    assert!(
        !fix.safe,
        "a fix that did not say must not be auto-applicable"
    );
    assert_eq!(
        (fix.start, fix.end, fix.replacement.as_str()),
        (0, 12, "const x = 3;"),
        "and the rest of the fix is unaffected by the field being absent"
    );
}

#[test]
fn the_no_message_form_records_a_report_with_no_message() {
    // `report`'s two `option` parameters are independent, which is the shape the TypeScript
    // API could only express as a union second argument.
    let reports = probe("const x = 1;", "bare");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].message, None);
    assert_eq!((reports[0].line, reports[0].column), (1, 1));
    assert_eq!(reports[0].fix, None);
}

#[test]
fn taking_reports_empties_the_context() {
    // Mirrors `lanekeep-js`'s test of the same name. Taken twice would be reported twice.
    let mut run = Run::new("const x = 1;", None);
    assert_eq!(run.probe("bare").len(), 1);
    assert!(run.take().is_empty(), "reports must not be reported twice");
}

#[test]
fn a_report_made_before_a_trap_survives_it() {
    // This test used to drive `today`, which trapped while it was declared and unimplemented.
    // Every `check-context` method is implemented now, so the trap it uses is `read-file` on a
    // context built with no file access — see `tests/reads.rs` for that decision. The property
    // asserted is unchanged and is about *reporting*: a handler may report several times and
    // then hit something the host cannot answer, and what it already found is still true.
    let mut run = Run::new("const x = 1;", None);
    let trap = run.call("trap-after-report").expect_err("the call traps");

    // The **root cause**, not the whole `{:?}` render, and the difference is not cosmetic.
    // wasmtime prefixes a host error with a wasm backtrace whose top frame is spelled
    // `wit-component:shim!indirect-lanekeep:host/types@0.1.0-[method]check-context.read-file`
    // — so an assertion over the rendered error contains the method name whatever the host
    // said, and holds even against a host that named a different method entirely. Measured on
    // the earlier version of this test: it survived a mutation replacing the name with
    // `something`.
    let cause = trap.root_cause().to_string();
    assert!(
        cause.contains("check-context.read-file"),
        "the trap names the method a reader has to go and look at: {cause}"
    );
    assert!(
        cause.contains("built without file access"),
        "and says why, rather than only that something failed: {cause}"
    );

    assert_eq!(
        rendered(&run.take()),
        ["1:1 recorded before the trap"],
        "discarding it would make the failure harder to diagnose rather than easier — the same \
         posture as `lanekeep-js`'s `a_rule_that_throws_still_leaves_earlier_reports`"
    );
}

// --- the date ------------------------------------------------------------------------------
//
// Every assertion below mirrors one `crates/lanekeep-js/src/host.rs` already makes —
// `today_is_what_the_host_supplied`, `today_is_absent_when_the_host_supplied_none`,
// `reading_today_is_observed_and_not_reading_it_is_not` — restated against the component
// boundary, plus the two cases that surface only here because WIT has no absent export.
//
// No test in this section reads a clock. The date is a literal the host hands the context, and
// the point of the whole mechanism is that a rule sees the run's date rather than the machine's.

#[test]
fn today_is_what_the_host_supplied() {
    // Verbatim, not re-rendered. `YYYY-MM-DD` comes from `lanekeep_core::suppression::Date`'s
    // `Display` by way of whoever fixes the date for the run; a host that reformatted it here
    // would make a component rule and the identical TypeScript rule compare against different
    // strings.
    assert_eq!(
        rendered(&Run::new("const x = 1;", Some("2026-08-01")).probe("today")),
        [r#"1:1 today=Some("2026-08-01")"#]
    );
}

#[test]
fn today_is_absent_when_the_host_supplied_none() {
    // `none`, not a trap and not an empty string. The world declares `option<string>` and
    // writes the meaning of absence into it — "when the rule is not permitted to observe it" —
    // so this is the boundary's own answer rather than a placeholder this host chose. It is
    // also the one shape `lanekeep-js` cannot produce: there `ctx.today` is simply absent and
    // reaching for it is a `TypeError`, because a JavaScript object can drop a property and a
    // WIT resource cannot drop a method.
    assert_eq!(
        rendered(&Run::new("const x = 1;", None).probe("today")),
        ["1:1 today=None"]
    );
}

#[test]
fn reading_today_is_observed_and_not_reading_it_is_not() {
    // The whole reason the date is allowed through at all. Without the second half every file
    // would look date-dependent and the corpus would re-check daily; without the first, a
    // result computed on one day would be served on the next — a date comparison frozen at
    // whenever the cache was written.
    let mut unread = Run::new(TWO_STATEMENTS, Some("2026-08-01"));
    assert_eq!(
        unread.probe("navigate").len(),
        5,
        "the probe that does not read the date still did real work"
    );
    assert!(!unread.date_was_read(), "nothing read the date");

    let mut read = Run::new(TWO_STATEMENTS, Some("2026-08-01"));
    assert_eq!(read.probe("today").len(), 1);
    assert!(read.date_was_read(), "the read was not observed");
}

#[test]
fn asking_for_a_date_the_host_does_not_have_is_still_an_observation() {
    // The case WIT creates and QuickJS cannot: the method exists whatever the host holds, so a
    // rule can ask and be told `none`. The flag is set anyway.
    //
    // The two mistakes are not symmetric. Recording an observation that did not need recording
    // dates one file's entry and costs a recompute whose answer is the same; not recording one
    // that did serves a stale answer indefinitely, and nothing announces it. Conditioning a
    // cache-soundness flag on host configuration is also the shape `src/host.rs`'s header warns
    // about for file access — output that depends on what the host granted, which is not one of
    // `(bytes, path, ruleset, config, tracked reads)` and so is in no key.
    let mut run = Run::new("const x = 1;", None);
    assert_eq!(
        rendered(&run.probe("today")),
        ["1:1 today=None"],
        "the host had no date to give"
    );
    assert!(
        run.date_was_read(),
        "the rule reached for the date surface, and that is what is recorded"
    );
}

#[test]
fn nothing_un_observes_a_read() {
    // Sticky for the life of the context, across taking reports and across a second `check`
    // that never mentions the date. Both halves matter under the per-file context shape: one
    // rule reading the date makes the *file* date-dependent, and a flag that a later rule or a
    // `take_reports` could clear would silently un-date the entry.
    let mut run = Run::new(TWO_STATEMENTS, Some("2026-08-01"));

    assert_eq!(run.probe("today").len(), 1);
    assert!(run.date_was_read(), "the read was observed to begin with");

    assert!(run.take().is_empty(), "the reports were already taken");
    assert!(
        run.date_was_read(),
        "taking reports empties the reports and not the observation"
    );

    assert_eq!(run.probe("navigate").len(), 5);
    assert!(
        run.date_was_read(),
        "a later rule that ignored the date does not make the file dateless again"
    );
}
