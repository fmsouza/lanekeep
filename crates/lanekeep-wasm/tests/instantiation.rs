//! Instantiation is lazy, it is reused, and it is bounded by (worker, rule).
//!
//! `runtime::MEMORY_RESERVATION`'s whole argument rests on that bound — about three hundred
//! and fifty instantiations for a run, against a 1.6× on guest compute that grows with the
//! corpus — and until this file existed the bound was a paragraph asking callers to behave.
//! What is asserted here is that the API makes it true: a [`Worker`] below is the shape
//! `lanekeep-engine` gives a rayon worker, and it cannot instantiate more than once per rule
//! however many files it is handed.
//!
//! # A laziness test is the easiest kind to write so that it cannot fail
//!
//! A counter that reads zero because nothing ran is not evidence that anything was deferred.
//! Three things guard against that here, and each one is the reason a test below is written
//! the way it is rather than the obvious way.
//!
//! **Every "instantiated nothing" case does work first.** The worker is handed files, walks
//! them, and reaches the end of its share; the assertion is that walking them instantiated
//! nothing. A test that built a runtime and asserted zero would pass against an engine that
//! had removed laziness *and* against one that had removed execution.
//!
//! **Every "instantiated once" case asserts what the guest reported as well as the count.**
//! Three files that all match one rule produce three reports and one instantiation. Without
//! the reports, an engine that silently skipped every file would pass.
//!
//! **The counter is the runtime's, not the harness's.** A counter a test harness increments
//! proves a property of the harness. [`WasmRuntime::instantiations`] is incremented in the one
//! place that instantiates, so there is nowhere for the two to disagree.
//!
//! The mutation each of these survives is the same one: moving instantiation out of
//! `WasmRuntime::rule` and into `WasmRuntime::for_rules` — which is how it would be written by
//! anyone who had not read `MEMORY_RESERVATION` — turns every count below from 0 or 1 into the
//! size of the rule set.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::sync::Arc;

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::ComponentLoader;
use lanekeep_wasm::WasmError;
use lanekeep_wasm::bindings::types;
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::load::{PermittedImports, instance_imports};
use lanekeep_wasm::runtime::{RuleSet, RuleSlot, WasmEngine, WasmRuntime};
use wasmtime::component::Resource;

/// The guest, which reports `"{file}: {captures}"` for every match it is given.
const WORLD_SHAPE: &[u8] = include_bytes!("fixtures/world-shape.wasm");

/// A component targeting a world of its own, for the case where resolution has to fail.
const SPIKE: &[u8] = include_bytes!("fixtures/spike.wasm");

/// The one artifact built for `wasm32-wasip1`, for the case where a permitted import is
/// nonetheless unbound.
const WASIP1: &[u8] = include_bytes!("fixtures/rejected/wasip1.wasm");

/// The run's shared half: one engine, one rule set, one clock.
///
/// Built once and shared by every [`Worker`], which is the arrangement the bound describes.
/// `RuleSet::add` is where a component's imports are resolved and type-checked
/// (`Linker::instantiate_pre`), and it happens here — once for the run — rather than once per
/// worker, because the answer cannot differ between stores.
struct Run {
    engine: Arc<WasmEngine>,
    rules: Arc<RuleSet>,
    clock: Arc<RunClock>,
    limits: Limits,
}

impl Run {
    /// A run with `count` rules in it, every one of them the same guest.
    ///
    /// The same component behind several slots is deliberate: what distinguishes two rules
    /// here is which `Option` holds their instance, and using one component keeps the test
    /// about that rather than about two guests behaving differently.
    fn with_rules(count: usize) -> Self {
        Self::with_limits(count, Limits::default())
    }

    fn with_limits(count: usize, limits: Limits) -> Self {
        let engine = WasmEngine::new().expect("the shipped configuration builds");
        // Through a loader, because `RuleSet::add` takes a `Loaded` and only a loader makes
        // one — which is what stops a caller reaching an instance without the import check.
        // `without_cache` here so this file writes nothing to disk; the artifact path is
        // `tests/load.rs`'s subject, not this one's.
        let loaded = ComponentLoader::without_cache()
            .load(&engine, "world-shape", WORLD_SHAPE)
            .expect("the fixture loads and imports only the declared world");
        let mut rules = RuleSet::new(&engine).expect("the world links");
        for n in 0..count {
            rules
                .add(format!("acme/rule-{n}"), &loaded)
                .expect("the fixture satisfies the world");
        }
        Self {
            engine,
            rules: Arc::new(rules),
            clock: RunClock::start(limits.global_timeout),
            limits,
        }
    }

    fn slot(&self, n: usize) -> RuleSlot {
        self.rules
            .slots()
            .nth(n)
            .expect("the rule set holds that many rules")
    }

    /// One more worker over the same rule set, exactly as rayon would give a run.
    fn worker(&self) -> Worker {
        Worker {
            runtime: WasmRuntime::for_rules(
                Arc::clone(&self.engine),
                Arc::clone(&self.rules),
                self.limits,
                Arc::clone(&self.clock),
            ),
            reports: Vec::new(),
        }
    }
}

/// One rayon worker: a store of its own, and the files it was given.
///
/// Mirrors `lanekeep-engine`'s `Worker`, which owns a `Sandbox` built on first use. Nothing is
/// instantiated when this is constructed, and that is not a detail: `rayon`'s `map_init` runs
/// its initializer per *chunk* rather than per thread, so a constructor that instantiated
/// would multiply by chunks — which for a small input is one per item.
struct Worker {
    runtime: WasmRuntime,
    /// Everything the guest reported, across every file this worker handled.
    reports: Vec<String>,
}

impl Worker {
    /// A file that hit the cache: nothing is checked, so nothing is instantiated.
    #[expect(
        clippy::unused_self,
        reason = "the point is that a cache hit reaches the runtime for nothing, which is \
                  only legible if the method is on the worker that would otherwise have"
    )]
    fn cache_hit(&mut self, _path: &str) {}

    /// A file that missed, with the rules whose queries matched it.
    ///
    /// An empty `matched` is a real case and not a degenerate one: a file that parsed, whose
    /// rules' queries found nothing. It costs a parse and no JavaScript under `lanekeep-js`,
    /// and it has to cost no instantiation here.
    fn checked(&mut self, path: &str, matched: &[(RuleSlot, &str)]) -> Result<(), WasmError> {
        let source = "const x = 1;\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let context = self
            .runtime
            .host_mut()
            .push_check_context(CheckContext::new(
                NodeArena::new(tree, source.to_owned()),
                path,
                Arc::new(TypeScript),
            ))
            .expect("the resource table accepts a context");

        for (slot, capture) in matched {
            let captures = vec![types::MatchEntry {
                name: (*capture).to_owned(),
                node: 0,
            }];
            self.runtime.check(*slot, &context, &captures)?;
        }

        // Read back what the guest reported for this file, exactly as the engine drains the
        // host after each file.
        let owned = Resource::new_own(context.rep());
        self.reports.extend(
            self.runtime
                .host_mut()
                .check_context_mut(&owned)
                .expect("the context is still live")
                .take_reports()
                .into_iter()
                .map(|report| report.message.unwrap_or_default()),
        );
        Ok(())
    }

    fn instantiations(&self) -> usize {
        self.runtime.instantiations()
    }
}

// --- laziness ---------------------------------------------------------------------------

/// A worker whose files all hit the cache instantiates nothing.
///
/// **The measured reason.** At twenty rules and fourteen workers, eager instantiation costs
/// 12,010 µs from `Component::from_file`, 12,115 µs from a byte slice and 10,967 µs on the
/// mapped path, against 135.2, 125.8 and 133.4 µs for creating the same fourteen stores and
/// instantiating nothing — a factor of 82 to 96 (medians, n = 30 per arm). The same-session
/// QuickJS baseline is a 34.6 ms warm run, so eager instantiation adds about a third to a warm
/// run in order to be able to do work a fully warm run never does.
///
/// The worker walks its whole share before anything is asserted, so a zero here is a zero
/// after the work rather than a zero instead of it.
#[test]
fn a_worker_whose_files_all_hit_the_cache_instantiates_nothing() {
    let run = Run::with_rules(3);
    let mut worker = run.worker();

    for path in ["src/a.ts", "src/b.ts", "src/c.ts", "src/d.ts"] {
        worker.cache_hit(path);
    }

    assert_eq!(
        worker.instantiations(),
        0,
        "a warm worker must not pay for rules it never runs"
    );
    for n in 0..3 {
        assert!(!worker.runtime.is_instantiated(run.slot(n)));
    }
}

/// And a worker whose files miss but match nothing also instantiates nothing.
///
/// The other half of the warm case, and the one a cache-based test would miss: the file was
/// read, parsed and checked, and every rule's query came back empty. Under `lanekeep-js` that
/// costs no JavaScript; here it has to cost no instance.
#[test]
fn a_worker_whose_queries_match_nothing_instantiates_nothing() {
    let run = Run::with_rules(3);
    let mut worker = run.worker();

    for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
        worker.checked(path, &[]).expect("checking a file returns");
    }

    assert_eq!(worker.instantiations(), 0);
    assert!(
        worker.reports.is_empty(),
        "and nothing was reported, because nothing ran"
    );
}

// --- reuse ------------------------------------------------------------------------------

/// Three files that all match one rule instantiate that rule once, not three times.
///
/// The alternative is priced rather than assumed cheap: a fresh instance per file per rule is
/// 40,000 instantiations against 280 for the same corpus and ruleset, measured at 331.5 ms
/// single-threaded — a different cost class by a factor of 143.
///
/// **The three reports are load-bearing.** Without them, an engine that skipped every file
/// would satisfy "instantiated once" just as well by instantiating once and doing nothing
/// with it.
#[test]
fn a_worker_instantiates_a_rule_once_however_many_files_match_it() {
    let run = Run::with_rules(1);
    let slot = run.slot(0);
    let mut worker = run.worker();

    for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
        worker
            .checked(path, &[(slot, "callee")])
            .expect("checking a file returns");
    }

    assert_eq!(
        worker.instantiations(),
        1,
        "the instance is built on first use and reused for every file after it"
    );
    assert_eq!(
        worker.reports,
        vec![
            "src/a.ts: callee".to_owned(),
            "src/b.ts: callee".to_owned(),
            "src/c.ts: callee".to_owned(),
        ],
        "and it really did run on all three, so the count is not one-because-nothing-happened"
    );
}

/// A rule that ran twice in one file is still one instantiation.
///
/// The per-(rule, match) call rather than the per-(rule, file) one: `check` is reached once
/// per query match, which for a real rule is many times per file.
#[test]
fn several_matches_in_one_file_are_still_one_instantiation() {
    let run = Run::with_rules(1);
    let slot = run.slot(0);
    let mut worker = run.worker();

    worker
        .checked(
            "src/a.ts",
            &[(slot, "first"), (slot, "second"), (slot, "third")],
        )
        .expect("checking a file returns");

    assert_eq!(worker.instantiations(), 1);
    assert_eq!(worker.reports.len(), 3);
}

/// **An `Option` per rule, not one flag per worker.**
///
/// This is the assertion that separates the two designs, and nothing else here does. One lazy
/// flag covering the ruleset recovers only the all-cache-hits case: the moment a single file
/// matches a single rule it builds every instance, so this test would see three where the
/// per-rule cache sees one. It pays in full for a worker that ran one rule.
#[test]
fn a_worker_instantiates_only_the_rules_that_actually_ran() {
    let run = Run::with_rules(3);
    let mut worker = run.worker();

    worker
        .checked("src/a.ts", &[(run.slot(1), "callee")])
        .expect("checking a file returns");

    assert_eq!(
        worker.instantiations(),
        1,
        "one flag per worker would have built all three here"
    );
    assert!(worker.runtime.is_instantiated(run.slot(1)));
    assert!(!worker.runtime.is_instantiated(run.slot(0)));
    assert!(!worker.runtime.is_instantiated(run.slot(2)));
}

// --- the bound ----------------------------------------------------------------------------

/// Every worker instantiates for itself, and once each: the bound is a product, not a total.
///
/// An instance belongs to a store and a store belongs to a worker, so fourteen workers running
/// twenty rules is two hundred and eighty instantiations rather than twenty. That product is
/// what `MEMORY_RESERVATION` is priced against, and this is the shape of it at two and one.
#[test]
fn the_bound_is_per_worker_per_rule_and_not_per_run() {
    let run = Run::with_rules(2);
    let slot = run.slot(0);

    let mut first = run.worker();
    let mut second = run.worker();

    for path in ["src/a.ts", "src/b.ts"] {
        first
            .checked(path, &[(slot, "callee")])
            .expect("checking a file returns");
    }
    for path in ["src/c.ts", "src/d.ts", "src/e.ts"] {
        second
            .checked(path, &[(slot, "callee")])
            .expect("checking a file returns");
    }

    assert_eq!(first.instantiations(), 1);
    assert_eq!(second.instantiations(), 1);
    assert_eq!(first.reports.len(), 2);
    assert_eq!(second.reports.len(), 3);
}

/// Resolution happens once, when a rule is added, and not once per worker.
///
/// `Linker::instantiate_pre` resolves and type-checks a component's imports independently of
/// how many stores go on to instantiate it, so the failure it can produce belongs to the rule
/// set rather than to any worker. Asserted with a component targeting a world of its own,
/// which is the case where that resolution has something to say.
#[test]
fn a_component_that_does_not_satisfy_the_world_is_refused_when_it_is_added() {
    let engine = WasmEngine::new().expect("the shipped configuration builds");
    // It loads: `spike.wasm` targets its own world and so imports nothing at all, which is
    // strictly less authority than the declared world rather than more. What it cannot do is
    // satisfy this one, and that is a separate question answered a line later.
    let spike = ComponentLoader::without_cache()
        .load(&engine, "acme/spike", SPIKE)
        .expect("importing nothing passes the import check");
    let mut rules = RuleSet::new(&engine).expect("the world links");

    let error = rules
        .add("acme/spike", &spike)
        .expect_err("a component targeting another world does not satisfy this one");
    assert!(matches!(error, WasmError::Engine(_)), "{error:?}");
    assert!(
        rules.is_empty(),
        "a rule that could not be resolved must not occupy a slot"
    );
}

/// Permitting an interface is not binding it, and the two fail closed independently.
///
/// `PermittedImports` decides what a component may *reach for*; the linker decides what the
/// host will *answer*. They are separate on purpose, and this is the direction that matters:
/// widening the permitted set alone grants nothing, because a component whose imports nobody
/// bound still cannot be instantiated. So a mistake in the permitted set is a refusal one step
/// later rather than an escape.
///
/// The Go authoring lane needs both halves widened together — `RuleSet::linker_mut` is the
/// other one — and needs to know that doing only the first is safe.
#[test]
fn permitting_an_interface_does_not_bind_it() {
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    // Permit literally everything the wrongly-targeted fixture reaches for, so the load-time
    // check has nothing to say about it.
    let mut permitted = PermittedImports::declared_world();
    let probe = engine
        .compile(WASIP1)
        .expect("the fixture is a valid component");
    for import in instance_imports(engine.engine(), &probe) {
        permitted = permitted.allowing(import);
    }

    let loaded = ComponentLoader::without_cache()
        .permitting(permitted)
        .load(&engine, "acme/wasip1", WASIP1)
        .expect("permitting everything it imports is what admitting it means");

    let mut rules = RuleSet::new(&engine).expect("the world links");
    let error = rules
        .add("acme/wasip1", &loaded)
        .expect_err("nothing bound `wasi:clocks/wall-clock`, so it cannot be resolved");
    assert!(matches!(error, WasmError::Engine(_)), "{error:?}");
    assert!(
        rules.is_empty(),
        "a rule that could not be resolved must not occupy a slot"
    );
}

/// The linker accepts bindings beyond the declared world, and the world still works.
///
/// Nothing in this crate needs this — a Rust rule on `wasm32-unknown-unknown` imports exactly
/// the host interface — but the Go lane does: TinyGo's runtime imports `wasi:random/random`,
/// `wasi:io/streams`, `wasi:io/error` and `wasi:cli/stdout` unconditionally, and those must be
/// *bound* rather than declined, since a declined import means the component never instantiates
/// at all. Without an accessor that lane cannot reach the linker and is blocked.
///
/// What is asserted is the seam, not WASI: an extra instance is bound, and a rule that does not
/// use it still resolves and runs. A binding that had clobbered `lanekeep:host/types@0.1.0`
/// would fail here rather than somewhere less obvious.
#[test]
fn the_linker_accepts_bindings_beyond_the_declared_world() {
    let engine = WasmEngine::new().expect("the shipped configuration builds");
    let loaded = ComponentLoader::without_cache()
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the fixture loads");

    let mut rules = RuleSet::new(&engine).expect("the world links");
    rules
        .linker_mut()
        .instance("acme:extra/thing@0.1.0")
        .expect("a fresh instance name is available")
        .func_wrap("noop", |_store, (): ()| Ok(()))
        .expect("the host can answer a function in it");

    let slot = rules
        .add("acme/rule", &loaded)
        .expect("the world still resolves with an extra instance bound beside it");

    let limits = Limits::default();
    let clock = RunClock::start(limits.global_timeout);
    let mut worker = Worker {
        runtime: WasmRuntime::for_rules(engine, Arc::new(rules), limits, clock),
        reports: Vec::new(),
    };
    worker
        .checked("src/a.ts", &[(slot, "callee")])
        .expect("and the rule runs");
    assert_eq!(worker.reports, vec!["src/a.ts: callee".to_owned()]);
    assert_eq!(worker.instantiations(), 1);
}

/// A slot from a different rule set is reported rather than panicked on.
///
/// A slot arrives from a caller, and the workspace denies panicking outside tests because an
/// engine that panics on a malformed input has failed at its job.
#[test]
fn a_slot_this_rule_set_never_issued_is_refused() {
    let three = Run::with_rules(3);
    let one = Run::with_rules(1);
    let mut worker = one.worker();

    let error = worker
        .checked("src/a.ts", &[(three.slot(2), "callee")])
        .expect_err("slot 2 does not exist in a set of one");
    assert!(matches!(error, WasmError::Engine(_)), "{error:?}");
    assert_eq!(worker.instantiations(), 0);
}

// --- what a per-file design would run into -------------------------------------------------

/// A `Store` never reclaims an instance, and lanekeep's own memory ceiling is what says so.
///
/// **This is the wall a per-file design hits, and it is not wasmtime's.** Under the shipped
/// default 64 MiB per-worker ceiling, one store accepts about sixty instances of this fixture
/// and then refuses — measured at exactly 60 on 2026-08-06 — because linear memories never
/// shrink and `MemoryCeiling` sums every grant a store has been given. Sixty is fifty-five
/// times sooner than wasmtime's own cap below, so a design that instantiated per file would
/// fail a real run on its sixty-first file rather than merely be slow.
///
/// The count is asserted as an order of magnitude rather than exactly, because it is a
/// property of the fixture's linear-memory minimum and a rebuilt fixture would move it. What
/// is exact is the variant: this is the ceiling, not the epoch.
#[test]
fn a_store_stops_accepting_instances_at_the_memory_ceiling_long_before_wasmtimes_cap() {
    let mut runtime = WasmRuntime::with_limits(Limits::default()).expect("the world links");
    let component = runtime
        .engine()
        .compile(WORLD_SHAPE)
        .expect("the fixture is a valid component");

    let mut instances = 0_usize;
    let error = loop {
        match runtime.instantiate(&component) {
            Ok(_) => instances += 1,
            Err(error) => break error,
        }
        assert!(
            instances < 5_000,
            "no limit fired at all, which means the ceiling is not reached by instantiation"
        );
    };

    assert!(
        matches!(error, WasmError::MemoryExceeded { .. }),
        "the ceiling is what stops it, not the clock: {error:?} after {instances} instances"
    );
    assert!(
        (1..200).contains(&instances),
        "measured at 60; a store that suddenly accepts {instances} has stopped accounting for \
         the memory its instances hold"
    );
}

/// And with the ceiling out of the way, wasmtime's own cap is exactly 3,333.
///
/// Three core instances per component instance against a default limit of 10,000, so the
/// 3,334th fails with `resource limit exceeded: instance count too high at 10001`. It is
/// deterministic rather than timing-dependent, which is why the number is asserted rather than
/// "it eventually fails" — a test that only asserted failure would pass against a store that
/// had started reclaiming instances, which is the change worth knowing about.
///
/// Three is a property of this artifact's structure rather than a constant of the component
/// model, so a rebuilt fixture could move this. The message says so.
///
/// Measured 2026-08-06, Apple M3 Max, `wasmtime` 47.0.3: 3,333 instances in 40.5 ms in the
/// release profile. It reserves about 13 TiB of address space on the way — 3,333 memories at
/// [`lanekeep_wasm::runtime::MEMORY_RESERVATION`] each — which is a reservation rather than a
/// commitment, but it is the reason this test is one test and not a loop somewhere hotter.
#[test]
fn a_store_never_reclaims_an_instance_and_wasmtime_stops_it_at_3333() {
    let mut runtime = WasmRuntime::with_limits(
        // Far above what 3,333 instances of this fixture hold, so the ceiling asserted in the
        // test above cannot be what fires here.
        Limits::default().with_memory_bytes(64 * 1024 * 1024 * 1024),
    )
    .expect("the world links");
    let component = runtime
        .engine()
        .compile(WORLD_SHAPE)
        .expect("the fixture is a valid component");

    let mut instances = 0_usize;
    let error = loop {
        match runtime.instantiate(&component) {
            Ok(_) => instances += 1,
            Err(error) => break error,
        }
        assert!(instances < 20_000, "no limit fired");
    };

    assert_eq!(
        instances, 3_333,
        "wasmtime's default cap is 10,000 core instances and this artifact costs three per \
         component instance; a different number means one of those two moved. It stopped with: \
         {error:?}"
    );
    assert!(
        matches!(&error, WasmError::Guest { message } if message.contains("instance count too high")),
        "and the cap is what stopped it, not the ceiling or the clock: {error:?}"
    );
}
