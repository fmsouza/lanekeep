//! End-to-end coverage for #193's typestate obligation dispatch.
//!
//! `RuleTester` runs the real `Engine`, so this is the first exercise of the whole pipeline:
//! `EXTRACT` carrying `obligation`/`checkObligation` off the rule module, `lanekeep-config`
//! loading it now that `dataflow` is implemented, `Engine::prepare` compiling the acquire and
//! release queries, and `Engine::run_rule`'s obligation arm calling into the analyzer and then
//! into `checkObligation` itself.
//!
//! Filed here rather than in `crates/lanekeep-engine/tests/`, where the design for this task
//! placed it: `lanekeep-testkit` depends on `lanekeep-engine`, so a dev-dependency the other
//! way would close a cycle in the publication order exactly as the comment beside
//! `lanekeep-engine`'s own `[dev-dependencies]` already describes for `lanekeep-rules` — see
//! that crate's `Cargo.toml`. `RuleTester`'s whole point is running the real engine end to
//! end, so the property under test is identical either way.
//!
//! `RULE` also pins Ruling 2 (the plan's `matches.is_empty()` guard) for free: it declares no
//! `query` and no `check`, so both tests below only ever reach a violation through the
//! obligation arm — there is no main-query match loop for either to fall back on.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. `tester()` below is neither, so the grant it already \
              makes for unit tests has to be restated for it."
)]

use lanekeep_testkit::RuleTester;

const RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/zeroed',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'zero it', remediation: 'rel(b) on all paths',\n\
              examples: { bad: 'const b = acq();', good: 'const b = acq(); rel(b);' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'missed on some path' : 'never released');\n\
      },\n\
    });\n";

fn tester() -> RuleTester {
    RuleTester::new("zeroed", RULE).expect("builds")
}

#[test]
fn reports_when_an_early_return_skips_release() {
    tester()
        .reports_messages(
            "function f(c) { const b = acq(); if (c) { return; } rel(b); }\n",
            &["missed on some path"],
        )
        .expect("early return leaves one path undischarged");
}

#[test]
fn silent_when_released_on_all_paths() {
    tester()
        .accepts("function f() { const b = acq(); rel(b); }\n")
        .expect("released on the only path");
}

/// `scope: 'block'`, run end to end. `crates/lanekeep-lang-js/src/obligation.rs` documents
/// the mechanism and carries the same two fixtures directly against `JsObligationAnalyzer`:
/// the analyzer resolves the acquire's nearest `statement_block` ancestor and keeps only
/// releases whose byte range sits inside it, so a release lexically after that block cannot
/// discharge an acquire inside it even though nothing here gives the two distinct control-flow
/// blocks. These two are the `RuleTester` equivalent, exercised through query matching,
/// `Engine::run_rule`'s obligation arm, and `checkObligation` itself.
const BLOCK: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/block-zero',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'block',\n\
      },\n\
      card: { message: 'not released before block exit', remediation: 'rel before leaving',\n\
              examples: { bad: '{ const b = acq(); }', good: '{ const b = acq(); rel(b); }' } },\n\
      checkObligation(ctx, u) { ctx.report(u.exit, 'left the block undischarged'); },\n\
    });\n";

fn block() -> RuleTester {
    RuleTester::new("block-zero", BLOCK).expect("builds")
}

#[test]
fn released_before_block_exit_is_silent() {
    block()
        .accepts("function f() { { const b = acq(); rel(b); } after(); }\n")
        .expect("released inside the block");
}

#[test]
fn released_after_the_block_reports() {
    block()
        .reports_messages(
            "function f() { { const b = acq(); } rel(b); }\n",
            &["left the block undischarged"],
        )
        .expect("the release is outside the block");
}

/// No top-level `query`, no `check` — only `obligation` and `checkObligation`. Pins two things
/// at once: that a rule built from only those two loads at all (the relaxations `build_rule`
/// grants an obligation-only rule), and that `Engine::run_rule`'s
/// `matches.is_empty() && obligation.is_none()` early return does not fire just because
/// `matches` is empty — with no main query, `matches` is *always* empty, so this can only ever
/// report through the obligation arm.
const ONLY: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/only',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"acq\")) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"rel\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'x', remediation: 'y', examples: { bad: 'acq()', good: 'acq(); rel()' } },\n\
      checkObligation(ctx, u) { ctx.report(u.exit, 'unmet'); },\n\
    });\n";

#[test]
fn an_obligation_only_rule_with_no_query_still_runs() {
    RuleTester::new("only", ONLY)
        .expect("builds")
        .reports_messages("function f() { const b = acq(); }\n", &["unmet"])
        .expect("no main query must not skip the obligation arm");
}

/// #193's real acceptance rule, `secrets-zeroed-on-all-paths`. Where `RULE` above uses bare
/// `acq`/`rel` calls to pin the wiring, this exercises a realistic acquire/release shape — a
/// member-call acquire (`e.getEntropy()`/`e.deriveSeed()`), a two-form release (`.fill(0)`
/// with a literal zero argument, or a `zeroBytes(...)` helper call) — against the seven
/// function-scope behaviors the feature's acceptance contract lists.
///
/// Namespaced `local/`, not the design doc's own illustrative `pera/`: `RuleTester`'s
/// generated `lanekeep.config.ts` declares no `namespaces`, and `rule_id::Namespace`'s two
/// built-ins — `lanekeep` and `local` — are exactly the ones that need no declaring. A
/// project's real `lanekeep.json` would declare `pera` and use it there; this fixture proves
/// the CFG/analyzer wiring, which the namespace does not touch.
const SECRETS: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/secrets-zeroed-on-all-paths',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (member_expression property: (property_identifier) @m) \
                    (#any-of? @m \"getEntropy\" \"deriveSeed\")) @acquire'],\n\
        release: ['(call_expression function: (member_expression property: (property_identifier) @p) \
                    (#eq? @p \"fill\") arguments: (arguments (number) @z) (#eq? @z \"0\")) @release',\n\
                  '(call_expression function: (identifier) @f (#eq? @f \"zeroBytes\")) @release'],\n\
        scope: 'function',\n\
      },\n\
      card: { message: 'secret buffer not zeroed on all paths',\n\
              remediation: 'call .fill(0) or zeroBytes on every path, e.g. in finally',\n\
              examples: { bad: 'const b = e.getEntropy();',\n\
                          good: 'const b = e.getEntropy(); try {} finally { b.fill(0); }' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'zeroed on some paths, not all' : 'never zeroed');\n\
      },\n\
    });\n";

fn secrets() -> RuleTester {
    RuleTester::new("secrets", SECRETS).expect("builds")
}

#[test]
fn happy_path_only_reports_partial() {
    secrets()
        .reports_messages(
            "function f(c) { const b = e.getEntropy(); if (c) { return; } b.fill(0); }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("early return skips the fill");
}

#[test]
fn zeroed_in_finally_is_silent() {
    secrets()
        .accepts(
            "function f() { const b = e.getEntropy(); try { use(b); } finally { b.fill(0); } }\n",
        )
        .expect("finally is on all paths");
}

#[test]
fn zeroed_only_after_throw_reports() {
    secrets()
        .reports_messages(
            "function f(c) { const b = e.getEntropy(); if (c) { throw x; } b.fill(0); }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("the throw path never zeroes");
}

#[test]
fn never_zeroed_reports_never() {
    secrets()
        .reports_messages(
            "function f() { const b = e.getEntropy(); }\n",
            &["never zeroed"],
        )
        .expect("no fill anywhere");
}

#[test]
fn zeroed_on_both_branches_is_silent() {
    secrets()
        .accepts("function f(c) { const b = e.getEntropy(); if (c) { b.fill(0); } else { b.fill(0); } }\n")
        .expect("both branches discharge");
}

#[test]
fn zeroed_in_a_maybe_zero_iteration_loop_reports() {
    secrets()
        .reports_messages(
            "function f(xs) { const b = e.getEntropy(); for (const x of xs) { b.fill(0); } }\n",
            &["zeroed on some paths, not all"],
        )
        .expect("a zero-iteration loop skips the fill");
}

#[test]
fn zero_bytes_helper_discharges() {
    secrets()
        .accepts("function f() { const b = e.getEntropy(); zeroBytes(b); }\n")
        .expect("the helper release form counts");
}

/// The acquire's `#any-of? @m "getEntropy" "deriveSeed"` has two literals; every fixture above
/// exercises only the first. `deriveSeed` is otherwise identical — a member-call acquire with
/// no release — so this pins the second literal without adding a new behavior to reason about.
#[test]
fn derive_seed_unzeroed_reports_never_zeroed() {
    secrets()
        .reports_messages(
            "function f() { const b = e.deriveSeed(); }\n",
            &["never zeroed"],
        )
        .expect("deriveSeed is the acquire's second #any-of? literal");
}

/// `AGENTS.md`'s determinism invariant, applied to the obligation arm specifically: "two runs
/// over identical input produce byte-identical output." Nothing about the CFG build, the
/// analyzer's witness search, or `checkObligation` should depend on anything but `source`.
#[test]
fn two_runs_are_byte_identical() {
    let t = secrets();
    let src = "function f(c) { const b = e.getEntropy(); if (c) { return; } b.fill(0); }\n";
    let a = t.run(src).expect("run a");
    let b = t.run(src).expect("run b");
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

/// The obligation value-identity feature's flagship pattern: register/forget correlated by
/// `@key` at `scope: 'module'`. Where `RULE`/`BLOCK`/`ONLY` above are un-keyed — any release
/// discharges any acquire — this binds `@key` on the call's argument identifier in both the
/// acquire and release queries, so discharge requires a release naming the *same* value,
/// anywhere in the file (module scope shares no control-flow graph across sibling
/// functions/arrows — see `crates/lanekeep-lang-js/src/obligation.rs`'s module-scope
/// short-circuit). `checkObligation` also reads `ctx.text(u.key)` to name the value in its
/// message, which is what pins `unmet.key` crossing the sandbox as a real node handle rather
/// than merely being present on the JS object.
const KEYED: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/registered-is-forgotten',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"reg\") \
                   arguments: (arguments (identifier) @key)) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"forget\") \
                   arguments: (arguments (identifier) @key)) @release'],\n\
        scope: 'module',\n\
      },\n\
      card: { message: 'not forgotten', remediation: 'call forget(id)',\n\
              examples: { bad: 'reg(a)', good: 'reg(a); forget(a)' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.acquire, `registration for ${ctx.text(u.key)} is never forgotten`);\n\
      },\n\
    });\n";

fn keyed() -> RuleTester {
    RuleTester::new("keyed", KEYED).expect("builds")
}

/// The analyzer-level RED for this exact fixture shape was established in Task 3
/// (`crates/lanekeep-lang-js/src/obligation.rs`'s module-scope key-matching unit tests); this
/// is its `RuleTester` equivalent, exercised through query matching, `Engine::run_rule`'s
/// obligation arm, and `checkObligation` itself — confirmed passing end to end, not
/// re-deriving the red. `on` and `off` are sibling arrow functions with no shared CFG; only
/// key correlation, not reachability, can discharge `reg(id)` here.
#[test]
fn a_matching_forget_in_a_sibling_arrow_is_silent() {
    keyed()
        .accepts("const on = (id) => { reg(id); };\nconst off = (id) => { forget(id); };\n")
        .expect("the sibling forget discharges it");
}

#[test]
fn a_missing_forget_reports_and_names_the_key() {
    keyed()
        .reports_messages(
            "const on = (id) => { reg(id); };\n",
            &["registration for id is never forgotten"],
        )
        .expect("no forget anywhere");
}

/// `scope: 'class'`, run end to end. Same acquire/release queries and `@key` correlation as
/// `KEYED` above; only the existence region changes, from the whole file to the enclosing
/// class. `crates/lanekeep-lang-js/src/obligation.rs`'s
/// `class_scope_is_silent_when_a_sibling_method_releases_the_same_key` and
/// `class_scope_reports_when_the_release_is_in_a_different_class` carry the same two fixture
/// shapes directly against `JsObligationAnalyzer`; these are the `RuleTester` equivalent,
/// exercised through query matching, `Engine::run_rule`'s obligation arm, and
/// `checkObligation` itself.
const CLASS: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/registered-is-forgotten-in-class',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"reg\") \
                   arguments: (arguments (identifier) @key)) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"forget\") \
                   arguments: (arguments (identifier) @key)) @release'],\n\
        scope: 'class',\n\
      },\n\
      card: { message: 'not forgotten', remediation: 'call forget(id) in the same class',\n\
              examples: { bad: 'class C { open() { reg(a); } }',\n\
                          good: 'class C { open() { reg(a); } close() { forget(a); } }' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.acquire, `registration for ${ctx.text(u.key)} is never forgotten`);\n\
      },\n\
    });\n";

fn class_scope() -> RuleTester {
    RuleTester::new("class-scope", CLASS).expect("builds")
}

/// A naive translation of `a_matching_forget_in_a_sibling_arrow_is_silent` above — only
/// `scope: 'module'` swapped for `scope: 'class'`, the fixture otherwise untouched — reports:
/// `on`/`off` are sibling arrow functions with no enclosing class at all, and `scope: 'class'`
/// requires one (`crates/lanekeep-lang-js/src/obligation.rs`'s
/// `class_scope_reports_an_acquire_with_no_enclosing_class`), so `reg(id)` is unconditionally
/// reported regardless of the matching `forget(id)` sitting elsewhere in the file — this is
/// the RED this suite's task report records, proof that `class` genuinely differs from
/// `module` rather than aliasing it. Wrapping both calls in one shared class, as below, is
/// what makes it discharge.
#[test]
fn a_matching_forget_in_a_sibling_method_of_the_same_class_is_silent() {
    class_scope()
        .accepts("class C { open() { reg(id); } close() { forget(id); } }\n")
        .expect("the sibling method's forget, in the same class, discharges it");
}

/// The invalid half of the pair above: `open`/`close` still sit in sibling methods with no
/// control-flow graph in common, but now in two different classes rather than one. The
/// matching key exists in the file — this is not `a_missing_forget_reports_and_names_the_key`'s
/// "no forget anywhere" case — but `scope: 'class'` bounds existence to the *same* class
/// (`crates/lanekeep-lang-js/src/obligation.rs`'s
/// `class_scope_reports_when_the_release_is_in_a_different_class`), so a release next door in
/// a sibling class cannot discharge it.
#[test]
fn a_forget_in_a_different_class_still_reports() {
    class_scope()
        .reports_messages(
            "class A { open() { reg(id); } }\nclass B { close() { forget(id); } }\n",
            &["registration for id is never forgotten"],
        )
        .expect("the only forget is in a different class");
}

/// `scope: 'component'`, run end to end. Same acquire/release queries and `@key` correlation
/// as `KEYED`/`CLASS` above; the existence region is the enclosing React function component —
/// a `PascalCase`-named function or arrow function whose body contains JSX.
/// `crates/lanekeep-lang-js/src/obligation.rs`'s `component_scope_is_silent_within_one_component`
/// and `component_scope_reports_across_two_components` carry the same two fixture shapes
/// directly against `JsObligationAnalyzer`; these are the `RuleTester` equivalent, exercised
/// through query matching, `Engine::run_rule`'s obligation arm, and `checkObligation` itself —
/// and, unlike the unit tests, through a real `.tsx` file on disk. That extension is not
/// cosmetic: `AGENTS.md` documents that which grammar parses a file is decided by the file's
/// extension, not by anything the rule declares, so a `.ts` fixture here would parse `<b/>`
/// under the plain TypeScript grammar rather than TSX — no JSX node would ever exist for
/// `contains_jsx` to find, `W`/`A`/`B` below would never be recognized as components, and
/// every case would report regardless of scope semantics. `RuleTester::with_extension` is
/// what selects the TSX grammar, mirroring `a_tsx_rule_can_be_tested_against_tsx` in
/// `crates/lanekeep-testkit/src/lib.rs`.
const COMPONENT: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/registered-is-forgotten-in-component',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"reg\") \
                   arguments: (arguments (identifier) @key)) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"forget\") \
                   arguments: (arguments (identifier) @key)) @release'],\n\
        scope: 'component',\n\
      },\n\
      card: { message: 'not forgotten', remediation: 'call forget(id) in the same component',\n\
              examples: { bad: 'function Foo() { reg(a); return <div/>; }',\n\
                          good: 'function Foo() { reg(a); forget(a); return <div/>; }' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.acquire, `registration for ${ctx.text(u.key)} is never forgotten`);\n\
      },\n\
    });\n";

fn component_scope() -> RuleTester {
    RuleTester::with_extension("component-scope", COMPONENT, "tsx").expect("builds")
}

/// A naive translation of `a_matching_forget_in_a_sibling_method_of_the_same_class_is_silent`
/// onto `component` scope: acquire and release both sit inside one `PascalCase` arrow function
/// whose body returns JSX (the release nested in an inner callback, exactly as in the unit
/// fixture), so `W` is a component region and the matching-key release discharges the
/// acquire — mirrors `crates/lanekeep-lang-js/src/obligation.rs`'s
/// `component_scope_is_silent_within_one_component`.
#[test]
fn a_matching_forget_in_the_same_component_is_silent() {
    component_scope()
        .accepts("const W = () => { reg(id); const c = () => { forget(id); }; return <b/>; };\n")
        .expect("acquire and release both sit inside the one PascalCase+JSX component W");
}

/// The invalid half: `A`/`B` are two separate `PascalCase`+JSX components with no component
/// region in common, so `B`'s release cannot discharge `A`'s acquire — mirrors
/// `crates/lanekeep-lang-js/src/obligation.rs`'s `component_scope_reports_across_two_components`.
#[test]
fn a_forget_in_a_different_component_still_reports() {
    component_scope()
        .reports_messages(
            "const A = () => { reg(id); return <b/>; };\n\
             const B = () => { forget(id); return <b/>; };\n",
            &["registration for id is never forgotten"],
        )
        .expect("the only forget is in a different component");
}

/// `keyBy: 'binding'`, run end to end through the real `Engine::prepare` path. Before this
/// suite, `crates/lanekeep-engine/src/lib.rs`'s `key_by == "binding"` → `KeyCorrelation::Binding`
/// mapping was reached only by `lanekeep-config`'s load-time refusals and by unit tests against
/// `JsObligationAnalyzer` directly — never by a real rule module going through config load,
/// query compilation and a match. `scope: 'function'`, not `'module'`: value-origin correlation
/// composes with the per-function CFG walk exactly as text correlation already does in `KEYED`
/// above (`Text`/`'module'`); this is the `Binding`/`'function'` combination.
///
/// The valid fixture below is `crates/lanekeep-lang-js/src/flow.rs`'s
/// `origin_follows_a_const_copy` unit fixture, unchanged: `id` is a plain copy of `clientId`, so
/// `register`'s `@key` and `forget`'s `@key` resolve to the same parameter under the value-origin
/// walk even though their captured text differs — that unit test proves the origins intersect;
/// this is its `RuleTester` equivalent, proving the whole pipeline discharges the obligation
/// because of it.
const KEYBY_BINDING: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/registered-is-forgotten-by-origin',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"register\") \
                   arguments: (arguments (identifier) @key)) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"forget\") \
                   arguments: (arguments (identifier) @key)) @release'],\n\
        scope: 'function',\n\
        keyBy: 'binding',\n\
      },\n\
      card: { message: 'not forgotten', remediation: 'call forget on the same value',\n\
              examples: { bad: 'register(a);', good: 'register(a); forget(a);' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'missed on some path' : 'never forgotten');\n\
      },\n\
    });\n";

fn keyby_binding() -> RuleTester {
    RuleTester::new("keyby-binding", KEYBY_BINDING).expect("builds")
}

#[test]
fn a_renamed_copy_correlates_under_keyby_binding() {
    keyby_binding()
        .accepts("function f(clientId) { const id = clientId; register(clientId); forget(id); }\n")
        .expect(
            "id is a copy of clientId; keyBy: 'binding' correlates them by shared value origin \
             even though their captured text differs",
        );
}

/// The invalid counterpart: two distinct parameters, never aliased, so their value origins are
/// two distinct root definitions (each parameter is its own root — see
/// `origin_of_identifier`'s "no reaching definition" case in `flow.rs`) that do not intersect.
/// `forget(otherId)`'s block is filtered out of the correlating-release set before the CFG walk
/// runs (`crates/lanekeep-lang-js/src/obligation.rs`'s `correlate`, called from the `rel_blocks`
/// filter in `analyze`), so the acquire is reported exactly as if no release existed at all —
/// `partial: false`, "never forgotten" — even though a syntactically identical `forget(...)` call
/// is right there.
#[test]
fn a_different_origin_still_reports_under_keyby_binding() {
    keyby_binding()
        .reports_messages(
            "function f(clientId, otherId) { register(clientId); forget(otherId); }\n",
            &["never forgotten"],
        )
        .expect("clientId and otherId are two distinct parameters with no shared origin");
}

/// `keyBy: 'text'` regression: the identical rule shape and the identical acquire/release
/// queries as `KEYBY_BINDING` above, differing only in `keyBy`. Pins that adding `'binding'`
/// left `'text'`'s pre-existing meaning — exact captured-text equality, nothing more —
/// untouched: this rule *reports* on exactly the source `KEYBY_BINDING` is silent on, because
/// `"clientId"` and `"id"` are different strings regardless of what either resolves to.
const KEYBY_TEXT: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/registered-is-forgotten-by-text',\n\
      requires: ['dataflow'],\n\
      obligation: {\n\
        acquire: ['(call_expression function: (identifier) @f (#eq? @f \"register\") \
                   arguments: (arguments (identifier) @key)) @acquire'],\n\
        release: ['(call_expression function: (identifier) @f (#eq? @f \"forget\") \
                   arguments: (arguments (identifier) @key)) @release'],\n\
        scope: 'function',\n\
        keyBy: 'text',\n\
      },\n\
      card: { message: 'not forgotten', remediation: 'call forget on the same value',\n\
              examples: { bad: 'register(a);', good: 'register(a); forget(a);' } },\n\
      checkObligation(ctx, u) {\n\
        ctx.report(u.exit, u.partial ? 'missed on some path' : 'never forgotten');\n\
      },\n\
    });\n";

fn keyby_text() -> RuleTester {
    RuleTester::new("keyby-text", KEYBY_TEXT).expect("builds")
}

#[test]
fn a_renamed_copy_does_not_correlate_under_keyby_text() {
    keyby_text()
        .reports_messages(
            "function f(clientId) { const id = clientId; register(clientId); forget(id); }\n",
            &["never forgotten"],
        )
        .expect(
            "the same source KEYBY_BINDING accepts; keyBy: 'text' compares captured text only, \
             and \"clientId\" != \"id\"",
        );
}

#[test]
fn matching_text_still_discharges_under_keyby_text() {
    keyby_text()
        .accepts("function f(a) { register(a); forget(a); }\n")
        .expect("equal captured text still correlates under keyBy: 'text', as before #258");
}
