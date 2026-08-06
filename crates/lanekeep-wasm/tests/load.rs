//! The load path: what a component may import, and where its compiled form comes from.
//!
//! Two fixtures, and the second is the whole reason this file is worth reading.
//! `tests/fixtures/world-shape.wasm` is built for `wasm32-unknown-unknown` and imports
//! exactly the declared world. `tests/fixtures/rejected/wasip1.wasm` is the same world,
//! implemented the same way, built for `cargo component`'s default `wasm32-wasip1` — and it
//! imports ten WASI interfaces on top, among them a wall clock, a filesystem and the process
//! environment.
//!
//! It is built rather than described because `AGENTS.md` records that the difference is
//! invisible at small scale: a guest that allocates nothing has *zero* imports on both
//! targets, so a check tested against a synthetic approximation would be tested against
//! nothing. [`the_wrongly_targeted_fixture_really_does_reach_ambient_authority`] is what keeps
//! that honest — if the artifact were ever rebuilt for the right target, every rejection test
//! below would still pass while asserting nothing, and that one would fail.
//!
//! # What each half has to survive
//!
//! **The import check has to be more than "it would not have instantiated anyway".** A
//! wrongly-targeted component fails to link today only because this host declines to provide
//! WASI, and that is a property of the host rather than of the artifact. The check is asserted
//! here to be a decision about a *set the caller supplies*:
//! [`naming_the_offending_interfaces_admits_the_same_artifact`] admits the very artifact every
//! other test rejects, and [`an_empty_permitted_set_rejects_a_correctly_targeted_component`]
//! rejects the one every other test admits. Between them the linker is not what decides.
//!
//! **The load path has to be more than "it worked".** Every assertion about it is on a
//! counter, because "the embedded slice is a fallback and never the load path" is only
//! checkable as a number: a run that quietly took the slice produces identical violations,
//! identical output and identical exit codes, and differs only in about 8× the resident memory
//! per instance.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` fires: nothing here panics directly, and an unfulfilled
// `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lanekeep_core::limits::{Limits, RunClock};
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types;
use lanekeep_wasm::host::CheckContext;
use lanekeep_wasm::load::{
    ComponentLoader, HOST_INTERFACE, LoadSource, Loaded, PermittedImports, check_imports,
    instance_imports,
};
use lanekeep_wasm::runtime::{RuleSet, WasmEngine, WasmRuntime};
use lanekeep_wasm::{WasmError, engine};
use wasmtime::component::{Component, Resource};

/// A component built for `wasm32-unknown-unknown`, as `just wasm-fixtures` builds every
/// artifact but one.
const WORLD_SHAPE: &[u8] = include_bytes!("fixtures/world-shape.wasm");

/// The one artifact built for `wasm32-wasip1`, on purpose.
///
/// `include_bytes!` is the wrong load path for a real rule — a byte slice cannot be mapped,
/// which forfeits wasmtime's copy-on-write memory images — and is right for a fixture that
/// exists to be refused.
const WASIP1: &[u8] = include_bytes!("fixtures/rejected/wasip1.wasm");

/// A second acceptable component, for the cases that need two.
///
/// It targets a world of its own and reaches no part of `std` that touches the WASI adapter, so
/// it imports nothing at all — strictly less authority than the declared world rather than
/// more, which is why the check admits it.
const SPIKE: &[u8] = include_bytes!("fixtures/spike.wasm");

/// The path the context reports as the file under check.
const FILE: &str = "src/example.ts";

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A cache directory of its own, removed on drop.
struct Cache {
    dir: PathBuf,
}

impl Cache {
    fn new(name: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-load-{name}-{}-{seq}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("creates the cache directory");
        Self { dir }
    }

    fn loader(&self) -> ComponentLoader {
        ComponentLoader::new(Some(self.dir.clone()))
    }

    /// Every `.cwasm` in the directory, sorted.
    fn artifacts(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .expect("the cache directory is readable")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "cwasm"))
            .collect();
        found.sort();
        found
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.dir));
    }
}

/// Compile a fixture without going through the loader, for the checks that are about
/// reflection rather than about loading.
fn compiled(bytes: &[u8]) -> (wasmtime::Engine, Component) {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, bytes).expect("the fixture is a valid component");
    (engine, component)
}

/// What the world-shape guest reports when it is run over one match, through the real host.
///
/// Takes a loaded component so a caller can point it at either load path and compare, and the
/// engine that component was loaded on — a `Component` belongs to its `Engine`, and handing
/// one to a store built on another fails with "cross-`Engine` instantiation is not currently
/// supported" rather than with anything about the component.
fn violations(engine: &Arc<WasmEngine>, loaded: &Loaded) -> Vec<String> {
    let engine = Arc::clone(engine);
    let mut rules = RuleSet::new(&engine).expect("the world links");
    let slot = rules
        .add("world-shape", loaded)
        .expect("the fixture satisfies the world");

    let limits = Limits::default();
    let clock = RunClock::start(limits.global_timeout);
    let mut runtime = WasmRuntime::for_rules(engine, Arc::new(rules), limits, clock);

    let source = "const x = 1;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parses");
    let context = runtime
        .host_mut()
        .push_check_context(CheckContext::new(
            NodeArena::new(tree, source.to_owned()),
            FILE,
            Arc::new(TypeScript),
        ))
        .expect("the resource table accepts a context");

    let captures = vec![types::MatchEntry {
        name: "callee".to_owned(),
        node: 0,
    }];
    runtime
        .check(slot, &context, &captures)
        .expect("the guest's per-file pass returns");

    let owned = Resource::new_own(context.rep());
    runtime
        .host_mut()
        .check_context_mut(&owned)
        .expect("the context is still live")
        .take_reports()
        .into_iter()
        .map(|report| report.message.unwrap_or_default())
        .collect()
}

// --- the import check ------------------------------------------------------------------

/// The fixture the rejection tests point at is genuinely a wrongly-targeted component.
///
/// **Without this every one of them could pass while asserting nothing.** If the artifact
/// were ever rebuilt for `wasm32-unknown-unknown` it would import only the declared world, the
/// rejections below would go on rejecting whatever they were handed by name, and nobody would
/// learn that the fixture stopped being a fixture. `AGENTS.md` records why that is a live
/// risk rather than a theoretical one: on this evidence the difference between the two targets
/// is ten interfaces, and on a guest that allocates nothing it is zero.
#[test]
fn the_wrongly_targeted_fixture_really_does_reach_ambient_authority() {
    let (engine, component) = compiled(WASIP1);
    let imports = instance_imports(&engine, &component);

    for reached in [
        "wasi:clocks/wall-clock@0.2.3",
        "wasi:filesystem/types@0.2.3",
        "wasi:filesystem/preopens@0.2.3",
        "wasi:cli/environment@0.2.3",
    ] {
        assert!(
            imports.iter().any(|import| import == reached),
            "the wasip1 fixture must reach `{reached}`, or it is not testing what it claims; \
             it imports {imports:?}"
        );
    }
    assert!(
        imports.iter().any(|import| import == HOST_INTERFACE),
        "and it must still be a rule: {imports:?}"
    );

    let (right_engine, right) = compiled(WORLD_SHAPE);
    assert_eq!(
        instance_imports(&right_engine, &right),
        vec![HOST_INTERFACE.to_owned()],
        "the same world on the right target imports the world and nothing else"
    );
}

#[test]
fn a_component_built_for_the_declared_world_is_accepted() {
    let (engine, component) = compiled(WORLD_SHAPE);
    check_imports(
        &engine,
        "world-shape",
        &component,
        &PermittedImports::declared_world(),
    )
    .expect("a component on the right target imports only the world");
}

/// The check refuses it, and the refusal names what it refused.
///
/// The rejection has to arrive from the check rather than from instantiation, because
/// instantiation only refuses it while *this* host declines to link WASI — a property of the
/// host, revocable by any embedder for any reason, and not a property of the artifact. The
/// signature is the evidence: `check_imports` is handed an engine and a component, and no
/// linker exists at the point it decides.
#[test]
fn a_component_built_for_wasip1_is_refused_by_the_check_that_names_what_it_refused() {
    let (engine, component) = compiled(WASIP1);

    let error = check_imports(
        &engine,
        "acme/no-console",
        &component,
        &PermittedImports::declared_world(),
    )
    .expect_err("a wasip1 component reaches outside the world");

    let WasmError::ForbiddenImports {
        name,
        imports,
        permitted,
    } = &error
    else {
        panic!("expected a forbidden-import refusal, got {error:?}");
    };

    assert_eq!(name, "acme/no-console");
    assert_eq!(
        permitted,
        &vec![HOST_INTERFACE.to_owned()],
        "the refusal has to say what was allowed as well as what was not"
    );
    assert!(
        imports.iter().any(|i| i == "wasi:clocks/wall-clock@0.2.3"),
        "{imports:?}"
    );
    assert!(
        imports.iter().any(|i| i == "wasi:filesystem/types@0.2.3"),
        "{imports:?}"
    );
    assert!(
        !imports.iter().any(|i| i == HOST_INTERFACE),
        "the world it legitimately imports must not be listed as an offense: {imports:?}"
    );

    let rendered = error.to_string();
    assert!(
        rendered.contains("wasm32-wasip1"),
        "the message has to name the cause a reader can act on: {rendered}"
    );
}

/// The same artifact, admitted, once the interfaces it reaches are named.
///
/// **This is the assertion that the check is a permitted set rather than an equality**, and it
/// is not generality for its own sake. Rust on `wasm32-unknown-unknown` imports exactly the
/// declared world and so does Python under `componentize-py --stub-wasi`, and Go cannot:
/// TinyGo's runtime imports `wasi:io/streams`, `wasi:cli/stdout` and `wasi:random/random`
/// unconditionally, so a Go component does not exist at all until the world includes
/// `wasi:cli/imports`. An equality check would reject every Go rule ever written.
///
/// It also settles what the previous test could not. If the rejection came from the host
/// declining to link WASI, widening a set the host never consults would change nothing.
#[test]
fn naming_the_offending_interfaces_admits_the_same_artifact() {
    let (engine, component) = compiled(WASIP1);

    let mut permitted = PermittedImports::declared_world();
    for import in instance_imports(&engine, &component) {
        permitted = permitted.allowing(import);
    }

    check_imports(&engine, "wasip1", &component, &permitted)
        .expect("naming every interface it reaches is what admitting it means");
}

/// And the check refuses a correct component when nothing is permitted.
///
/// The other half of the pair above: a check that admitted everything would pass every
/// acceptance test in this file, and this is the one it would fail.
#[test]
fn an_empty_permitted_set_rejects_a_correctly_targeted_component() {
    let (engine, component) = compiled(WORLD_SHAPE);

    let error = check_imports(
        &engine,
        "world-shape",
        &component,
        &PermittedImports::none(),
    )
    .expect_err("nothing is permitted, so nothing is admitted");

    assert!(
        matches!(&error, WasmError::ForbiddenImports { imports, .. }
            if imports == &vec![HOST_INTERFACE.to_owned()]),
        "{error:?}"
    );
    assert!(error.to_string().contains("permitted: nothing"), "{error}");
}

/// A refusal is not a limit breach, so nothing treats it as a budget problem.
#[test]
fn a_refusal_is_a_rule_problem_and_not_a_budget_problem() {
    let (engine, component) = compiled(WASIP1);
    let error = check_imports(
        &engine,
        "wasip1",
        &component,
        &PermittedImports::declared_world(),
    )
    .expect_err("refused");
    assert!(!error.is_limit_breach());
}

/// Resource-type imports are not instance imports, and counting them rejects every rule.
///
/// `wasm-tools component wit` shows one import on the world-shape artifact; wasmtime's own
/// view of the same bytes lists three, the extra two being the bare `check-context` and
/// `reduce-context` *resource type* imports the component model requires because those types
/// appear in an export signature. A check written as `imports().len() == 1` over wasmtime's
/// list therefore rejects every component that takes a context, which is every rule.
#[test]
fn only_instance_imports_are_counted() {
    let (engine, component) = compiled(WORLD_SHAPE);
    let raw = component.component_type().imports(&engine).count();
    assert_eq!(raw, 3, "wasmtime's raw list carries the two resource types");
    assert_eq!(
        instance_imports(&engine, &component).len(),
        1,
        "and exactly one of them is an instance the guest can call into"
    );
}

// --- the load path ---------------------------------------------------------------------

/// A writable cache directory means the mapped path, and the fallback counter stays at zero.
///
/// The counter is the assertion. A run that quietly took the slice produces identical
/// violations, identical output and identical exit codes, and differs only in about 8× the
/// resident memory per instance — so there is nothing else about it a test could notice.
#[test]
fn a_run_with_a_writable_cache_directory_never_touches_the_slice() {
    let cache = Cache::new("writable");
    let engine = WasmEngine::new().expect("the shipped configuration builds");
    let loader = cache.loader();

    let mapped = loader
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the fixture loads");

    assert_eq!(mapped.source(), LoadSource::Mapped);
    assert_eq!(
        loader.embedded_loads(),
        0,
        "condition 2: never the slice when a mapped file is available"
    );
    assert_eq!(loader.mapped_loads(), 1);
    assert_eq!(
        loader.compilations(),
        1,
        "a cold cache compiles once — and still ends up mapped"
    );
    assert_eq!(
        cache.artifacts().len(),
        1,
        "and it wrote the artifact it then mapped"
    );
}

/// The second run compiles nothing.
///
/// This is the 252× figure in one assertion: 186 ms to compile twenty components against
/// 0.74 ms to load twenty precompiled ones, and 186 ms is 23% of the entire 800 ms cold
/// budget, spent before a file is read.
#[test]
fn a_second_run_maps_what_the_first_compiled() {
    let cache = Cache::new("reused");
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    let first = cache.loader();
    first
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the first run loads");
    assert_eq!(first.compilations(), 1);

    // A fresh loader over the same directory: a second process, as far as anything here can
    // tell.
    let second = cache.loader();
    let loaded = second
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the second run loads");

    assert_eq!(loaded.source(), LoadSource::Mapped);
    assert_eq!(
        second.compilations(),
        0,
        "the whole point of writing the artifact is that the next run does not compile"
    );
    assert_eq!(second.mapped_loads(), 1);
    assert_eq!(second.embedded_loads(), 0);
    assert_eq!(
        cache.artifacts().len(),
        1,
        "and it reused the file rather than writing a second"
    );
}

/// A component changes, so its artifact does. There is no invalidation step to get wrong.
#[test]
fn a_different_component_gets_a_different_artifact() {
    let cache = Cache::new("addressed");
    let engine = WasmEngine::new().expect("the shipped configuration builds");
    let loader = cache.loader();

    loader
        .load(&engine, "rule", WORLD_SHAPE)
        .expect("the first component loads");
    // The same name and different bytes, which is exactly what an edited rule looks like.
    // `spike.wasm` is a different component that also passes the import check — it targets a
    // world of its own and so imports nothing at all.
    loader
        .load(&engine, "rule", SPIKE)
        .expect("the edited component loads too");

    assert_eq!(
        cache.artifacts().len(),
        2,
        "content-addressing means the second never overwrote the first"
    );

    // And the first is still usable, which is what "never overwrote" has to mean. A cache that
    // kept two files but had corrupted one would pass the count above.
    let warm = cache.loader();
    warm.load(&engine, "rule", WORLD_SHAPE)
        .expect("the first component is still there");
    assert_eq!(warm.compilations(), 0, "it was read back, not rebuilt");
}

/// No cache directory, so the fallback — and it is counted rather than silent.
#[test]
fn a_run_with_no_cache_directory_takes_the_fallback() {
    let engine = WasmEngine::new().expect("the shipped configuration builds");
    let loader = ComponentLoader::without_cache();

    let fallback = loader
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the fixture still loads");

    assert_eq!(fallback.source(), LoadSource::Embedded);
    assert_eq!(loader.embedded_loads(), 1);
    assert_eq!(loader.mapped_loads(), 0);
    assert_eq!(
        loader.compilations(),
        1,
        "which is the cost the fallback pays on every run"
    );
}

/// A cache directory that cannot be created is a fallback, not a failure.
///
/// The read-only container image, the sandboxed CI step. The failure mode this is guarding
/// against is a tool that stops working where it used to work: today `.lanekeep/` holds
/// results and a run without one still works, and under this shape it also holds code.
///
/// **The directory is made uncreatable by putting a file where its parent has to be**, rather
/// than by `chmod`. A read-only directory is not read-only to root, and enough CI runs in a
/// container as root that a `chmod`-based test would pass by not testing anything there. This
/// one fails identically for every user on every platform.
#[test]
fn a_cache_directory_that_cannot_be_created_takes_the_fallback() {
    let cache = Cache::new("unwritable");
    let blocker = cache.dir.join("not-a-directory");
    std::fs::write(&blocker, b"a file where a directory has to be").expect("writes the blocker");

    let engine = WasmEngine::new().expect("the shipped configuration builds");
    let loader = ComponentLoader::new(Some(blocker.join("components")));

    let fallback = loader
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("an unwritable cache directory must not fail the run");

    assert_eq!(fallback.source(), LoadSource::Embedded);
    assert_eq!(loader.embedded_loads(), 1);
    assert_eq!(loader.mapped_loads(), 0);
}

/// The fallback produces the same violations as the mapped path.
///
/// The two differ in residency and instantiation cost and in nothing a rule can observe. If
/// they ever differed in what a rule reports, the fallback would be a second engine rather
/// than a second load path.
#[test]
fn both_load_paths_produce_the_same_violations() {
    let cache = Cache::new("agree");
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    let mapped_loader = cache.loader();
    let mapped = mapped_loader
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the mapped path loads");
    assert_eq!(mapped.source(), LoadSource::Mapped);

    let embedded_loader = ComponentLoader::without_cache();
    let embedded = embedded_loader
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the fallback loads");
    assert_eq!(embedded.source(), LoadSource::Embedded);

    let from_mapped = violations(&engine, &mapped);
    assert_eq!(
        from_mapped,
        vec![format!("{FILE}: callee")],
        "the guest ran and reported through the real host"
    );
    assert_eq!(
        violations(&engine, &embedded),
        from_mapped,
        "the fallback is a load path and not a second engine"
    );
}

/// An artifact that cannot be loaded is discarded and recompiled, not trusted.
///
/// Truncation is the reachable case — a process killed mid-write, a full disk — and a stale
/// one is the common case: wasmtime records its version, the target triple and twenty-six
/// compiled tunables in every `.cwasm` and refuses one whose values disagree, which is
/// precisely how a `MEMORY_RESERVATION` change is meant to invalidate every artifact rather
/// than be mapped anyway.
#[test]
fn an_artifact_that_cannot_be_loaded_is_recompiled_rather_than_trusted() {
    let cache = Cache::new("corrupt");
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    let first = cache.loader();
    first
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the first run writes an artifact");
    let artifact = cache
        .artifacts()
        .pop()
        .expect("the first run wrote exactly one");
    std::fs::write(&artifact, b"not a precompiled component").expect("corrupts it");

    let second = cache.loader();
    let loaded = second
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("a corrupt artifact must not fail the run");

    assert_eq!(loaded.source(), LoadSource::Mapped);
    assert_eq!(second.compilations(), 1, "it recompiled");
    assert_eq!(
        second.embedded_loads(),
        0,
        "and it did not give up and use the slice"
    );

    // And it repaired what it found, so the next run is warm again rather than recompiling
    // forever.
    let third = cache.loader();
    third
        .load(&engine, "world-shape", WORLD_SHAPE)
        .expect("the third run loads");
    assert_eq!(third.compilations(), 0);
}

/// The loader refuses a forbidden component on both paths, and writes nothing for it.
///
/// The second half is the reason the check runs before `persist` rather than after. An
/// artifact written for a component that will never run is wasted work and a file a reader
/// would have to explain — and it would sit in the cache directory looking exactly like a
/// legitimate one.
#[test]
fn the_loader_refuses_a_forbidden_component_however_it_was_loaded() {
    let cache = Cache::new("refused");
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    let mapped = cache.loader();
    let error = mapped
        .load(&engine, "wasip1", WASIP1)
        .expect_err("the loader applies the check");
    assert!(
        matches!(error, WasmError::ForbiddenImports { .. }),
        "{error:?}"
    );
    assert!(
        cache.artifacts().is_empty(),
        "a refused component must leave nothing behind: {:?}",
        cache.artifacts()
    );

    let embedded = ComponentLoader::without_cache();
    let error = embedded
        .load(&engine, "wasip1", WASIP1)
        .expect_err("and applies it on the fallback too");
    assert!(
        matches!(error, WasmError::ForbiddenImports { .. }),
        "{error:?}"
    );
}

/// A warm artifact is checked again rather than vouched for by the run that wrote it.
///
/// The mapped path is the one where the component bytes are never looked at, so a check that
/// only ran on the compile path would leave a whole class of run — every warm one — deciding
/// nothing. Here the first run is permitted everything and writes the artifact; the second
/// permits only the world and has to refuse the very file the first left behind.
#[test]
fn a_warm_artifact_is_checked_again_and_not_taken_on_trust() {
    let cache = Cache::new("recheck");
    let engine = WasmEngine::new().expect("the shipped configuration builds");

    let mut everything = PermittedImports::declared_world();
    let (reflect, component) = compiled(WASIP1);
    for import in instance_imports(&reflect, &component) {
        everything = everything.allowing(import);
    }

    let permissive = cache.loader().permitting(everything);
    let loaded = permissive
        .load(&engine, "wasip1", WASIP1)
        .expect("this loader permits what it reaches");
    assert_eq!(loaded.source(), LoadSource::Mapped);
    assert_eq!(cache.artifacts().len(), 1, "and the artifact is on disk");

    let strict = cache.loader();
    let error = strict
        .load(&engine, "wasip1", WASIP1)
        .expect_err("a stricter run must not inherit the earlier run's verdict");
    assert!(
        matches!(error, WasmError::ForbiddenImports { .. }),
        "{error:?}"
    );
    assert_eq!(
        strict.compilations(),
        0,
        "and it reached that verdict off the mapped artifact, having compiled nothing"
    );
}
