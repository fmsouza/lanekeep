//! What a rule can still reach once `componentize-js` has put it inside StarlingMonkey.
//!
//! # The finding this file exists for
//!
//! A component built with `jco componentize --disable all` has **exactly one import**,
//! `lanekeep:host/types@0.1.0`. Verified at every level: all 29 of its core-module imports are
//! lanekeep's. No clock, no randomness, no filesystem, no network.
//!
//! And yet, driven through lanekeep's own host on 2026-08-10, a rule inside one observed:
//!
//! ```text
//! Date.now=1786352655014 ; Math.random=0.48401551228016615 ;
//! newDate=2026-08-10T09:04:15.014Z ; fetch=function ; setTimeout=function
//! ```
//!
//! Byte-identical across two calls in one process and two processes minutes apart.
//! `Date.now()` returns the instant the component was *built*, frozen into the `wizer` memory
//! image; `Math.random()` returns a constant.
//!
//! **Determinism survives.** The component's bytes are a `ruleset_hash` input, so even the
//! frozen timestamp is part of the cache key, and two runs over identical input still produce
//! identical output.
//!
//! **The sandbox invariant does not.** `AGENTS.md` says these are absent — "not *restricted* —
//! they are absent from the context" — and `crates/lanekeep-js/src/sandbox.rs` delivers that
//! under QuickJS with an intrinsic allowlist that omits `Date`, a `delete Math.random` for what
//! the allowlist cannot reach, and explicit attention to `performance.now()`. Present and
//! silently frozen is worse than either absence or failure.
//!
//! # Why the probe list is derived rather than written here
//!
//! The repair is a list of names in `packages/lanekeep/runtime/host.js`, and the thing that
//! goes wrong with a list is that it stops matching the one it was copied from. So nothing here
//! names a global: [`withheld_by_quickjs`] reads what `sandbox.rs` asserts is absent, and the
//! two tests below hold both the JavaScript list *and* the built component to it. A name added
//! to `sandbox.rs` is probed here without this file being touched, and a name dropped from
//! `host.js` fails before anything is rebuilt.
//!
//! `crates/lanekeep-js/tests/host_types.rs` and `crates/lanekeep-js/tests/resolver_parity.rs`
//! are the precedents: two artifacts that must agree, read as text and compared, rather than a
//! third list somebody maintains.
//!
//! # One instantiation
//!
//! Every component-driven assertion is in a single `#[test]`, deliberately. Compiling a 12.4 MiB
//! StarlingMonkey component takes **96 s** in the dev profile and 6 s in release — Cranelift
//! itself is what is unoptimized — so a second test here that loaded it would add another 96 s
//! rather than a few seconds.
//!
//! One test outside this file does pay it anyway: `world_shape.rs`'s glob over
//! `tests/fixtures/*.wasm` compiles this artifact too, which is the import-list equality check
//! and is worth what it costs. Those two are the tail of `just check` now, and every further
//! artifact in that directory is charged its compile time twice.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else. The helpers below are neither.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types::{EmittedFact, MatchEntry};
use lanekeep_wasm::host::{CheckContext, ReduceContext, Report};
use lanekeep_wasm::{
    ComponentLoader, Loaded, Resource, RuleSet, RuleSlot, WasmEngine, WasmError, WasmRuntime,
};

/// The QuickJS sandbox's own source, read at compile time so this cannot be pointed at a stale
/// copy.
const SANDBOX: &str = include_str!("../../lanekeep-js/src/sandbox.rs");

/// The component runtime's glue module, on the same terms.
const HOST_JS: &str = include_str!("../../../packages/lanekeep/runtime/host.js");

/// The file the fixture's harness parses, and the path it is checked under.
const SOURCE: &str = "const x = 1;\n";
const FILE: &str = "src/a.ts";

/// The rules `tests/fixtures/js-globals/rule.js` registers, in the order it registers them.
///
/// Asserted against the component's own `rules()` below rather than trusted: an index that
/// slipped would run a different rule than the one a failure named.
///
/// `probe/order` is last and comes from a *second module*, which is the whole of its point —
/// see [`the_runtime_is_evaluated_before_any_rule_module`].
const IDS: [&str; 5] = [
    "probe/reach",
    "probe/context",
    "probe/cross",
    "probe/throw",
    "probe/order",
];

// --- the two claims -----------------------------------------------------------------------

/// Every global QuickJS withholds is withheld by the glue module too.
///
/// Text only, so it costs nothing and fails before the expensive test below even loads the
/// component — which is the right order for a disagreement that is a typo.
#[test]
fn the_glue_module_withholds_what_quickjs_withholds() {
    let expected = withheld_by_quickjs();
    let actual = withheld_by_host_js();

    let missing: Vec<&String> = expected.difference(&actual).collect();
    let extra: Vec<&String> = actual.difference(&expected).collect();

    assert!(
        missing.is_empty(),
        "`packages/lanekeep/runtime/host.js` does not withhold {missing:?}, which \
         `crates/lanekeep-js/src/sandbox.rs` asserts is absent under QuickJS. A rule would \
         reach it in a component and not in the sandbox, which is the drift these two lists \
         exist to make impossible."
    );
    assert!(
        extra.is_empty(),
        "`packages/lanekeep/runtime/host.js` withholds {extra:?}, which \
         `crates/lanekeep-js/src/sandbox.rs` does not assert is absent. Either the sandbox has \
         a hole this list already knew about — add the assertion there — or the component is \
         withholding something a rule may legitimately use in the other engine."
    );
}

/// And the component actually built from it withholds them at run time.
///
/// Reading the list from `sandbox.rs` rather than from `host.js` is what makes this more than a
/// restatement of the test above: a name could be in `host.js`'s array and survive deletion, or
/// be reachable by another spelling, and only running the thing can say.
#[test]
fn the_component_withholds_the_clock_and_randomness() {
    let mut probe = Probe::new();

    assert_eq!(probe.rule_ids(), IDS, "the fixture registered other rules");

    // Which passes a rule has is the component's own answer, and the engine dispatches on it.
    // `probe/reach` is check-only and `probe/cross` is reduce-only, so a `hasCheck` that
    // answered from the world's shape rather than from the rule would agree with itself on
    // both and be wrong on both.
    for (rule, check, reduce) in [(0, true, false), (2, false, true)] {
        assert_eq!(probe.has_check(rule), check, "hasCheck({rule})");
        assert_eq!(probe.has_reduce(rule), reduce, "hasReduce({rule})");
    }

    the_whole_global_surface_is_the_one_that_was_reviewed(&mut probe);

    // The five the brief names, spelled as a rule author would reach them, and then everything
    // `sandbox.rs` withholds. The first five are here explicitly because they are the ones the
    // measurement was taken on, and a derivation that stopped covering them should be loud.
    let named = [
        "Date.now()",
        "Math.random()",
        "new Date()",
        "performance.now()",
        "fetch('http://example.invalid')",
    ];
    let derived: Vec<String> = withheld_by_quickjs()
        .iter()
        .map(|name| reach(name))
        .collect();

    let mut probes: Vec<String> = named.iter().map(|probe| (*probe).to_owned()).collect();
    probes.extend(derived);

    // A loop over nothing would assert nothing, and the list is derived — so its emptiness is a
    // failure of the derivation rather than a fact about the sandbox.
    assert!(
        probes.len() > 20,
        "only {} probes: the derivation from sandbox.rs has stopped finding names",
        probes.len()
    );

    for expression in &probes {
        let (outcome, reports) = probe.check(0, expression);

        let error = outcome.err().unwrap_or_else(|| {
            panic!(
                "`{expression}` must not be reachable from inside a component, and it was: {}",
                reports
                    .first()
                    .and_then(|report| report.message.clone())
                    .unwrap_or_else(|| "<the rule reported nothing>".to_owned())
            )
        });

        // The *reason*, not merely that it failed. A probe that failed because the fixture was
        // missing, or because the store was poisoned by an earlier one, would pass a weaker
        // assertion while proving nothing about the global it was written for.
        let rendered = error.to_string();
        assert!(
            rendered.contains("is not a function") || rendered.contains("is not defined"),
            "`{expression}` failed for the wrong reason: {rendered}"
        );
    }

    the_runtime_is_evaluated_before_any_rule_module(&mut probe);
    the_rest_of_the_glue_module(&mut probe);
}

/// A rule's *module scope* is evaluated after `withhold()` has run, and not merely its handlers.
///
/// `packages/lanekeep/runtime/entry.js` calls `withhold()` at its own top level and documents
/// the requirement this creates: a generated entry must import the runtime **before** any rule
/// module, because ES modules evaluate depth-first in the order their imports are written. A
/// rule imported first would have its module body run while `Date` was still reachable and could
/// close over it — and nothing afterwards would notice, since `check` sees the same withheld
/// globals either way.
///
/// # Why this needs a second module, and why the fixture did not have one
///
/// Every other rule here is defined in `rule.js`, the module that imports the runtime. Those
/// cannot be evaluated before the import at the top of their own file whatever anyone writes, so
/// they demonstrate nothing about the ordering — a claim made and withdrawn during B2.
/// `probe.js` is a second module, imported after the runtime, and it records what it could see
/// while it was being evaluated.
///
/// # And why a bundler makes it worth building rather than reasoning about
///
/// The ordering rule is a property of ES modules; what actually ships is a *bundle*. rolldown
/// flattens every module into one before `wizer` runs, and this is where its having preserved
/// the order is checked. Verified in the failing direction on 2026-08-10 by moving
/// `import order from './probe.js'` above the runtime import and rebuilding: the report came
/// back `Date=function ; Math.random=function ; fetch=function ; setTimeout=function` and this
/// assertion failed. The order in the source is carried into the bundle, and the bundle is what
/// makes it matter.
fn the_runtime_is_evaluated_before_any_rule_module(probe: &mut Probe) {
    let (outcome, reports) = probe.check(4, "p");
    outcome.expect("the ordering probe runs");

    let seen = reports
        .first()
        .and_then(|report| report.message.clone())
        .expect("the ordering probe reports what its module scope could see");

    assert_eq!(
        seen, "Date=undefined ; Math.random=undefined ; fetch=undefined ; setTimeout=undefined",
        "a rule module was evaluated before the runtime withheld the clock, so its module scope \
         could have captured one. The generated entry must import `lanekeep/runtime/entry` \
         before any rule — see `packages/lanekeep/runtime/entry.js`."
    );
}

/// Every name a rule can still see, pinned, so that a toolchain bump cannot add one quietly.
///
/// # Why the two tests above do not already cover this
///
/// They are both *denial* checks: given a list of names, prove each one is unreachable. Neither
/// can notice a name arriving. And the parity check is an equality, so `host.js` cannot
/// pre-emptively withhold a StarlingMonkey-only hazard without a matching assertion in
/// `sandbox.rs` — which nobody would write for a global they have never heard of.
///
/// `Temporal` is the concrete case rather than a hypothetical. SpiderMonkey ships it, this build
/// does not have it, and `Temporal.Now.instant()` is a clock. If a `componentize-js` bump turns
/// it on, every other assertion in this file stays green and rules gain a clock.
///
/// This is the same claim the load-time import check makes about the component's *imports*, made
/// about what its runtime provides — an import list that says "no clock" while the engine hands
/// one out is exactly the gap this whole file exists for.
///
/// # When this fails
///
/// A **removed** name is nothing: drop it from the string. An **added** one is a decision, and
/// the question is whether it is a capability or a source of nondeterminism. If it is, it goes
/// into `sandbox.rs`'s absence assertions and into `host.js`'s `WITHHELD` — and then it is not in
/// this list, because it will have been deleted. Only after answering that does the string move.
fn the_whole_global_surface_is_the_one_that_was_reviewed(probe: &mut Probe) {
    /// Sorted, comma-separated, as `Object.getOwnPropertyNames(globalThis)` reports it after
    /// `withhold()` has run. Measured 2026-08-10: jco 1.27.0, componentize-js 0.22.0.
    const SURVIVING: &str = "AbortController,AbortSignal,AggregateError,Array,ArrayBuffer,\
        AsyncDisposableStack,BigInt,BigInt64Array,BigUint64Array,Blob,Boolean,\
        ByteLengthQueuingStrategy,CompressionStream,CountQueuingStrategy,CustomEvent,\
        DOMException,DataView,DecompressionStream,DisposableStack,Error,EvalError,Event,\
        EventTarget,File,Float16Array,Float32Array,Float64Array,FormData,\
        Function,Infinity,Int16Array,Int32Array,Int8Array,InternalError,Iterator,JSON,Map,Math,\
        MultipartFormData,NaN,Number,Object,Promise,Proxy,RangeError,\
        ReadableByteStreamController,ReadableStream,ReadableStreamBYOBReader,\
        ReadableStreamBYOBRequest,ReadableStreamDefaultController,ReadableStreamDefaultReader,\
        ReferenceError,Reflect,RegExp,Set,String,SuppressedError,Symbol,SyntaxError,TextDecoder,\
        TextEncoder,TransformStream,TypeError,URIError,URL,URLSearchParams,Uint16Array,\
        Uint32Array,Uint8Array,Uint8ClampedArray,WeakMap,WeakSet,WritableStream,addEventListener,\
        atob,btoa,decodeURI,decodeURIComponent,dispatchEvent,encodeURI,encodeURIComponent,escape,\
        eval,globalThis,isFinite,isNaN,parseFloat,parseInt,queueMicrotask,removeEventListener,\
        structuredClone,undefined,unescape";

    let (outcome, reports) =
        probe.check(0, "Object.getOwnPropertyNames(globalThis).sort().join(',')");
    outcome.expect("enumerating the globals is not itself withheld");

    let reported = reports
        .first()
        .and_then(|report| report.message.clone())
        .expect("the probe reports what it enumerated");
    let seen = reported
        .rsplit_once("string:")
        .map(|(_, names)| names)
        .expect("the probe reports the value it evaluated to");

    let expected: BTreeSet<&str> = SURVIVING.split(',').collect();
    let actual: BTreeSet<&str> = seen.split(',').collect();

    let arrived: Vec<&&str> = actual.difference(&expected).collect();
    let gone: Vec<&&str> = expected.difference(&actual).collect();

    assert!(
        arrived.is_empty(),
        "the component's engine now provides {arrived:?}, which nothing in this repository has \
         looked at. Decide whether each is a capability or a source of nondeterminism before \
         adding it to the string in this function: if it is, it belongs in \
         `crates/lanekeep-js/src/sandbox.rs`'s absence assertions and in `host.js`'s WITHHELD \
         instead, and it will then not be here at all."
    );
    assert!(
        gone.is_empty(),
        "the component's engine no longer provides {gone:?}. Nothing is at risk — a name that is \
         not there cannot be reached — so drop it from the string in this function."
    );
}

/// Everything else `host.js` and `entry.js` do, driven through the same instance.
///
/// A separate function rather than a separate `#[test]`, and the reason is arithmetic: compiling
/// this component takes 96 s in the dev profile, so a second test that loaded it would double
/// the suite's wall clock. A separate function rather than more of the one above, because that
/// one was 101 lines and clippy allows 100 — which is a better reason than it sounds, since the
/// two halves assert unrelated things and only share the instance.
fn the_rest_of_the_glue_module(probe: &mut Probe) {
    let (outcome, reports) = probe.check(1, "p");
    outcome.expect("the context probe runs");
    let seen = reports
        .first()
        .and_then(|report| report.message.clone())
        .expect("the context probe reports what it saw");

    for expected in [
        // `configure` reached the factory with the options the set was built with.
        "note=hello",
        // Properties, which are functions across the boundary and properties to a rule.
        "filePath=src/a.ts",
        "fileText=\"const x = 1;\\n\"",
        "root=0",
        // `option<T>` arrives as `undefined`, not as null and not as an empty object.
        "parent=undefined",
        "bindingKind=undefined",
        "closest=undefined",
        "today=undefined",
        // Navigation.
        "kind=program",
        "isNamed=true",
        "line=1",
        "column=1",
        "children=1",
        "namedChildren=1",
        "ancestors=0",
        "isShadowed=false",
        // A match crosses as a list of entries and reaches a rule as captures keyed by name.
        "subtree=1:d",
        "loc={\"file\":\"src/a.ts\",\"line\":1,\"column\":1}",
    ] {
        assert!(
            seen.contains(expected),
            "the check context lost `{expected}`: {seen}"
        );
    }

    // --- the cross-file half -------------------------------------------------------------

    let reports = probe.reduce(2);
    let said = reports
        .first()
        .and_then(|report| report.message.clone())
        .expect("the reduce probe reports what it saw");
    assert!(
        said.contains("files=a.ts+b.ts"),
        "the file list did not arrive: {said}"
    );
    // Merged, with the host's `file` overriding whatever the payload claimed — a rule cannot
    // misattribute a violation to a file it did not come from.
    assert!(
        said.contains(r#"export=[{"kind":"export","file":"a.ts"}]"#),
        "facts did not arrive merged and filtered: {said}"
    );
    assert!(
        said.contains(r#""file":"b.ts"#),
        "the second fact is missing: {said}"
    );

    // --- a throw carries its message and its stack ------------------------------------------

    let (outcome, _) = probe.check(3, "p");
    let error = outcome.expect_err("the throwing rule fails");
    let rendered = error.to_string();
    assert!(
        rendered.contains("a rule that failed on purpose"),
        "a caught throw lost its message: {rendered}"
    );
    assert!(
        rendered.contains("inner"),
        "a caught throw lost its stack — the graceful channel is what makes remapping possible \
         at all, and an empty `frames` is indistinguishable from a trap: {rendered}"
    );

    // --- and a rule that refuses its options says so rather than trapping --------------------

    // `probe/reach` exports a rule object, so there is nowhere for options to go: they reach a
    // rule by being closed over, which is what a factory is for. Ignoring them silently is the
    // failure `AGENTS.md` records against `no-unwrap`'s `allow` — an ignored option only ever
    // *adds* violations, and the author assumes their own pattern is wrong.
    let refused = probe
        .configured_with("probe/reach", 0, r#"{"note":"nowhere to put this"}"#)
        .expect_err("a rule object handed options refuses them");

    assert!(
        matches!(refused, WasmError::Misconfigured { .. }),
        "a refused configuration must be the graceful channel and not a trap: {refused:?}"
    );
    assert!(
        refused.to_string().contains("probe/reach"),
        "the refusal has to name which rule was misconfigured: {refused}"
    );
}

// --- extraction -----------------------------------------------------------------------------

/// Every global `crates/lanekeep-js/src/sandbox.rs` withholds, as it spells them.
///
/// Two sources, because the sandbox withholds in two ways and both have to be read:
///
/// - `BOOTSTRAP`'s `delete` statements, for what the intrinsic allowlist cannot reach.
/// - The absence assertions in its test module, which are the executable statement of what the
///   allowlist leaves out — `Date` and `performance` are never *installed*, so there is no
///   deletion naming them and the tests are the only place they appear.
///
/// Whitespace is removed entirely before matching, which is what makes the patterns below
/// survive rustfmt moving an argument onto its own line. Comments go first, or removing the
/// newlines would fold the rest of a line into a `//`.
///
/// # Three shapes, and a refusal to grow a fourth
///
/// An absence can be asserted in more ways than this reads, and one of them used to be in that
/// file: `assert_eq!(s.eval::<String>("typeof Math.random"), "undefined", …)` says exactly what
/// `type_of(&s, "Math.random")` says and is invisible here. A name added in that shape would be
/// withheld under QuickJS and reachable in a component with every test green — and the anchor
/// check below does not catch it, since the anchors are drawn from the shapes that *are* read.
///
/// The fix was to delete the shape rather than to parse it, and [`no_fourth_shape`] is what
/// keeps it deleted. A smarter extractor is a worse answer: it has to be right about a language,
/// where the guard only has to be right about one string.
fn withheld_by_quickjs() -> BTreeSet<String> {
    let source = uncommented(SANDBOX);
    no_fourth_shape(&source);
    let mut names = BTreeSet::new();

    // --- what the bootstrap deletes ---
    let bootstrap = source
        .split_once("constBOOTSTRAP:&str=r\"")
        .and_then(|(_, rest)| rest.split_once("\";"))
        .map(|(body, _)| body)
        .expect("sandbox.rs declares a BOOTSTRAP script");
    for (index, _) in bootstrap.match_indices("delete") {
        let rest = &bootstrap[index + "delete".len()..];
        let Some((target, _)) = rest.split_once(';') else {
            continue;
        };
        names.insert(target.trim_start_matches("globalThis.").to_owned());
    }

    // --- what the tests assert is absent ---
    for (index, _) in source.match_indices("forglobalin[") {
        let rest = &source[index + "forglobalin[".len()..];
        let Some((list, after)) = rest.split_once(']') else {
            continue;
        };
        // `assert_ne!` in the same shape is the *presence* test for what rules do need, so the
        // comparison has to be part of the pattern rather than the call.
        if !after.starts_with("{assert_eq!(type_of(&s,global),\"undefined\"") {
            continue;
        }
        names.extend(literals(list));
    }
    for (index, _) in source.match_indices("assert_eq!(type_of(&s,\"") {
        let rest = &source[index + "assert_eq!(type_of(&s,\"".len()..];
        let Some((name, after)) = rest.split_once('"') else {
            continue;
        };
        if after.starts_with("),\"undefined\"") {
            names.insert(name.to_owned());
        }
    }

    // The derivation is the load-bearing part of both tests above, and a rewrite of `sandbox.rs`
    // that broke it would leave them green over an empty set. These five span all three sources:
    // an allowlist omission, a deletion, and a loop.
    for anchor in ["Date", "performance", "Math.random", "fetch", "crypto"] {
        assert!(
            names.contains(anchor),
            "the extraction from sandbox.rs found {} names and not `{anchor}`: it no longer \
             reads what that file withholds, and every claim built on it is vacuous",
            names.len()
        );
    }
    names
}

/// Refuse an absence asserted in a shape [`withheld_by_quickjs`] cannot read.
///
/// Keyed on the *comparison* rather than on the call, because `s.eval` is ordinary in that file
/// — `assert_eq!(s.eval::<i32>("1 + 1")…, 2)` is how it checks that the engine works at all.
/// What makes an assertion an absence is that it compares against `"undefined"`, so every such
/// comparison is found and the enclosing macro walked back to; only the ones reached through
/// `s.eval` are refused.
///
/// `assert_ne!` has to be one of the openers even though it is never the offender: it is the
/// *presence* test for what rules do need, and walking past it would attribute its `"undefined"`
/// to whichever `assert_eq!` came earlier in the file — which is a `s.eval` call in
/// `evaluates_ordinary_javascript`, and a false positive that would be very hard to read.
///
/// `source` is already uncommented and whitespace-free, so prose cannot trip this.
fn no_fourth_shape(source: &str) {
    for (index, _) in source.match_indices(",\"undefined\"") {
        let before = &source[..index];
        let opener = before
            .rfind("assert_eq!(")
            .max(before.rfind("assert_ne!("))
            .unwrap_or(0);

        assert!(
            !before[opener..].contains("s.eval::"),
            "sandbox.rs asserts an absence through `s.eval` rather than through `type_of`:\n\n    \
             {}…\n\n\
             That shape is invisible to the extraction in this file, so a name withheld by it \
             would be reachable inside a WebAssembly component with nothing going red. Write it \
             as `assert_eq!(type_of(&s, \"the.name\"), \"undefined\", …)` — `type_of` evaluates \
             `typeof (the.name)`, so a dotted name needs nothing special.",
            before[opener..].chars().take(70).collect::<String>()
        );
    }
}

/// Every global `packages/lanekeep/runtime/host.js` deletes.
///
/// Its `WITHHELD` array is data rather than a sequence of `delete` statements precisely so that
/// this can read it, and so that the array a test reads is the array the code runs.
fn withheld_by_host_js() -> BTreeSet<String> {
    let array = HOST_JS
        .split_once("export const WITHHELD = [")
        .and_then(|(_, rest)| rest.split_once("\n]"))
        .map(|(body, _)| body)
        .expect("host.js exports a WITHHELD array");

    let names: BTreeSet<String> = literals(&uncommented(array)).collect();
    assert!(
        !names.is_empty(),
        "host.js's WITHHELD array parsed as empty, so both directions of the comparison are \
         vacuous"
    );
    names
}

/// Source with its `//` comments taken out and every run of whitespace removed.
///
/// Line comments first: removing the newlines afterwards would otherwise fold whatever follows
/// a `//` into it and swallow the code this reads.
fn uncommented(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect()
}

/// Every double-quoted or single-quoted literal in a fragment, in order.
fn literals(fragment: &str) -> impl Iterator<Item = String> + '_ {
    fragment
        .split(['"', '\''])
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
}

/// A withheld name, spelled the way a rule would reach it.
///
/// A dotted name is a property of something that still exists — `Math` is not going anywhere —
/// so reaching it is a *call*, and the failure is "is not a function". A bare name is a global
/// reference, and the failure is "is not defined". Both are what the same mistake looks like in
/// the QuickJS sandbox, which is the point of the exercise.
fn reach(name: &str) -> String {
    if name.contains('.') {
        format!("{name}()")
    } else {
        name.to_owned()
    }
}

// --- the harness ------------------------------------------------------------------------------

/// The fixture, loaded once, with every rule it hosts in a slot.
struct Probe {
    engine: Arc<WasmEngine>,
    loaded: Loaded,
    runtime: WasmRuntime,
    slots: Vec<RuleSlot>,
}

impl Probe {
    fn new() -> Self {
        let engine = WasmEngine::new().expect("an engine");
        let bytes = std::fs::read("tests/fixtures/js-globals.wasm").expect("the artifact ships");
        let loaded = ComponentLoader::without_cache()
            .load(&engine, "js-globals", &bytes)
            .expect("a component built with --disable all imports nothing but the host");

        let mut set = RuleSet::new(&engine).expect("a set");
        let slots = IDS
            .iter()
            .enumerate()
            .map(|(index, id)| {
                // Only the context probe is a factory, and it is the one given options — so a
                // `configure` that never reached the factory shows up as `note=undefined`
                // rather than as nothing at all.
                let options = if *id == "probe/context" {
                    r#"{"note":"hello"}"#
                } else {
                    "null"
                };
                let index = u32::try_from(index).expect("four rules");
                set.add(*id, &loaded, index, options).expect("added")
            })
            .collect();

        let limits = Limits::default();
        let clock = RunClock::start(limits.global_timeout);
        let runtime = WasmRuntime::for_rules(Arc::clone(&engine), Arc::new(set), limits, clock);
        Self {
            engine,
            loaded,
            runtime,
            slots,
        }
    }

    /// Configure one rule with options and hand back what the component said.
    ///
    /// A second `RuleSet` over the *same* `Loaded`, so nothing is compiled again — only
    /// instantiated. `configure` runs on the way to an instance and there is no door that
    /// re-runs it, which is what makes "configured once, before any check" a property of the
    /// type rather than of whoever remembers to call something.
    fn configured_with(&self, id: &str, index: u32, options: &str) -> Result<(), WasmError> {
        let mut set = RuleSet::new(&self.engine)?;
        let slot = set.add(id, &self.loaded, index, options)?;
        let limits = Limits::default();
        let clock = RunClock::start(limits.global_timeout);
        let mut runtime =
            WasmRuntime::for_rules(Arc::clone(&self.engine), Arc::new(set), limits, clock);
        runtime.rule(slot).map(|_| ())
    }

    /// What the component says it hosts.
    fn rule_ids(&mut self) -> Vec<String> {
        self.slots
            .iter()
            .map(|slot| {
                self.runtime
                    .metadata(*slot)
                    .expect("metadata is readable")
                    .id
            })
            .collect()
    }

    /// Whether the component says this rule has a per-file pass.
    fn has_check(&mut self, rule: usize) -> bool {
        self.runtime
            .has_check(self.slots[rule])
            .expect("the component answers")
    }

    /// Whether the component says this rule has a cross-file pass.
    fn has_reduce(&mut self, rule: usize) -> bool {
        self.runtime
            .has_reduce(self.slots[rule])
            .expect("the component answers")
    }

    /// Run one rule's per-file pass with a single capture of the given name, and take whatever
    /// it reported.
    fn check(&mut self, rule: usize, capture: &str) -> (Result<(), WasmError>, Vec<Report>) {
        let context = self.context();
        let captures = vec![MatchEntry {
            name: capture.to_owned(),
            node: 0,
        }];
        let outcome = self.runtime.check(self.slots[rule], &context, &captures);
        let reports = self
            .runtime
            .host_mut()
            .take_check_context(context)
            .expect("the context is still live")
            .take_reports();
        (outcome, reports)
    }

    /// Run one rule's cross-file pass over two files and two facts.
    fn reduce(&mut self, rule: usize) -> Vec<lanekeep_wasm::host::ReduceReport> {
        let context = self
            .runtime
            .host_mut()
            .push_reduce_context(ReduceContext::new(
                vec!["a.ts".to_owned(), "b.ts".to_owned()],
                vec![
                    EmittedFact {
                        kind: "export".to_owned(),
                        file: "a.ts".to_owned(),
                        // Claiming a file it did not come from, so the merge can be seen to
                        // override rather than merely to add.
                        data: r#"{"kind":"export","file":"lies.ts"}"#.to_owned(),
                    },
                    EmittedFact {
                        kind: "import".to_owned(),
                        file: "b.ts".to_owned(),
                        data: r#"{"kind":"import"}"#.to_owned(),
                    },
                ],
            ))
            .expect("the resource table accepts a context");

        self.runtime
            .reduce(self.slots[rule], &context)
            .expect("the cross-file pass runs");
        self.runtime
            .host_mut()
            .take_reduce_context(context)
            .expect("the context is still live")
            .take_reports()
    }

    /// A check context over one trivial file.
    fn context(&mut self) -> Resource<CheckContext> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("the grammar loads");
        let tree = parser.parse(SOURCE, None).expect("it parses");

        self.runtime
            .host_mut()
            .push_check_context(CheckContext::new(
                NodeArena::new(tree, SOURCE.to_owned()),
                FILE,
                Arc::new(TypeScript),
            ))
            .expect("the resource table accepts a context")
    }
}
