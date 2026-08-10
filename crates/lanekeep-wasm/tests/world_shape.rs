//! The world in `wit/world.wit` is satisfiable from both sides.
//!
//! `tests/fixtures/world-shape/` is a component that exports all six of the `rule` world's
//! functions; this file implements the host half of the same world and links the two
//! together. Neither half proves much alone — a world nobody can target is a document, and a
//! world no host can implement is the same document from the other end.
//!
//! **This is not a rule and the host here is not the host.** Every value the stub returns is
//! a constant and nothing reads the context it was handed. What is under test is the *shape*:
//! that the generated traits can be implemented, that a host-owned context can be lent to a
//! guest export as a `borrow<>`, and that records, options and lists of records survive the
//! canonical ABI in both directions. `tests/navigation.rs` is where the real host is
//! exercised.
//!
//! The one thing that has to be real here is the value pushed into the table.
//! `check-context`'s representation is [`lanekeep_wasm::host::CheckContext`], which owns a
//! parsed file — so [`context`] parses one. A representation that could be conjured without a
//! tree would be a context that could claim to be reading a file it does not have.

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::types::{
    BindingKind, CheckContext, EmittedFact, FactError, Fix, Host, HostCheckContext,
    HostReduceContext, NodeLocation, ReadError, ReduceContext, ReduceLocation,
};
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::load::{HOST_INTERFACE, PermittedImports, check_imports, instance_imports};
use wasmtime::Store;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, HasSelf, Linker, Resource, ResourceTable};

/// The component under test, as built by `just wasm-fixtures`.
///
/// `include_bytes!` is the wrong load path for a rule — a byte slice cannot be mapped, which
/// forfeits wasmtime's copy-on-write memory images — and is fine for a fixture that is never
/// precompiled and never shipped.
const WORLD_SHAPE: &[u8] = include_bytes!("fixtures/world-shape.wasm");

/// The path the stub reports as the file under check.
const FILE: &str = "src/lib.ts";

/// The store's data: the table the contexts live in, and what the guest reported through
/// them.
#[derive(Default)]
struct StubHost {
    table: ResourceTable,
    /// Every `report` the guest made, per-file and cross-file alike, in call order.
    reported: Vec<String>,
}

impl HostCheckContext for StubHost {
    fn file_path(&mut self, _: Resource<CheckContext>) -> wasmtime::Result<String> {
        Ok(FILE.to_owned())
    }

    fn file_text(&mut self, _: Resource<CheckContext>) -> wasmtime::Result<String> {
        Ok("const x = 1;\n".to_owned())
    }

    fn root(&mut self, _: Resource<CheckContext>) -> wasmtime::Result<u32> {
        // Zero, and that is the whole reason `closest-ancestor` returns an option: anything
        // testing this handle for truthiness discards the root of every file.
        Ok(0)
    }

    fn kind(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<String>> {
        Ok(Some("program".to_owned()))
    }

    fn text(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<String>> {
        Ok(None)
    }

    fn is_named(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<bool> {
        Ok(true)
    }

    fn line(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<u32>> {
        Ok(Some(0))
    }

    fn column(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<u32>> {
        Ok(Some(0))
    }

    fn parent(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<u32>> {
        Ok(None)
    }

    fn children(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Vec<u32>> {
        Ok(Vec::new())
    }

    fn named_children(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Vec<u32>> {
        Ok(Vec::new())
    }

    fn ancestors(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Vec<u32>> {
        Ok(Vec::new())
    }

    fn resolves_to_import(
        &mut self,
        _: Resource<CheckContext>,
        _: u32,
        _: String,
        _: Option<String>,
    ) -> wasmtime::Result<bool> {
        Ok(false)
    }

    fn is_imported_from(
        &mut self,
        _: Resource<CheckContext>,
        _: u32,
        _: String,
    ) -> wasmtime::Result<bool> {
        Ok(false)
    }

    fn binding_kind(
        &mut self,
        _: Resource<CheckContext>,
        _: u32,
    ) -> wasmtime::Result<Option<BindingKind>> {
        Ok(Some(BindingKind::Const))
    }

    fn is_shadowed(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<bool> {
        Ok(false)
    }

    fn query_subtree(
        &mut self,
        _: Resource<CheckContext>,
        _: u32,
        _: String,
    ) -> wasmtime::Result<Result<Vec<types::Match>, String>> {
        Ok(Ok(Vec::new()))
    }

    fn closest_ancestor(
        &mut self,
        _: Resource<CheckContext>,
        _: u32,
        _: String,
    ) -> wasmtime::Result<Result<Option<types::Match>, String>> {
        Ok(Ok(None))
    }

    fn read_file(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
    ) -> wasmtime::Result<Result<Option<String>, ReadError>> {
        Ok(Ok(None))
    }

    fn file_exists(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
    ) -> wasmtime::Result<Result<bool, ReadError>> {
        Ok(Ok(false))
    }

    fn emit_fact(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
        _: String,
    ) -> wasmtime::Result<Result<(), FactError>> {
        Ok(Ok(()))
    }

    fn loc(&mut self, _: Resource<CheckContext>, _: u32) -> wasmtime::Result<Option<NodeLocation>> {
        Ok(Some(NodeLocation {
            file: FILE.to_owned(),
            line: 0,
            column: 0,
        }))
    }

    fn report(
        &mut self,
        _: Resource<CheckContext>,
        node: u32,
        message: Option<String>,
        fix: Option<Fix>,
    ) -> wasmtime::Result<()> {
        self.reported.push(format!(
            "check node={node} message={} fix={}",
            message.unwrap_or_default(),
            fix.map_or_else(|| "none".to_owned(), |f| f.text)
        ));
        Ok(())
    }

    fn today(&mut self, _: Resource<CheckContext>) -> wasmtime::Result<Option<String>> {
        Ok(None)
    }

    fn drop(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

impl HostReduceContext for StubHost {
    fn files(&mut self, _: Resource<ReduceContext>) -> wasmtime::Result<Vec<String>> {
        Ok(vec!["a.ts".to_owned(), "b.ts".to_owned()])
    }

    fn facts(
        &mut self,
        _: Resource<ReduceContext>,
        _: Option<String>,
    ) -> wasmtime::Result<Vec<EmittedFact>> {
        Ok(vec![EmittedFact {
            kind: "stub".to_owned(),
            file: "a.ts".to_owned(),
            data: "{}".to_owned(),
        }])
    }

    fn report(
        &mut self,
        _: Resource<ReduceContext>,
        at: ReduceLocation,
        message: Option<String>,
    ) -> wasmtime::Result<()> {
        self.reported.push(format!(
            "reduce file={} line={:?} message={}",
            at.file,
            at.line,
            message.unwrap_or_default()
        ));
        Ok(())
    }

    fn drop(&mut self, this: Resource<ReduceContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

impl Host for StubHost {}

/// Builds everything a call needs: an engine, the fixture, a linker carrying the stub host,
/// and a store.
///
/// A crate-level `expect` is what licenses the `expect` calls here. `clippy.toml`'s
/// `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]` modules; a helper
/// in an integration-test crate is neither, so the identical line that passes inside a test
/// body fails the gate out here.
#[expect(
    clippy::expect_used,
    reason = "a helper in a tests/ crate is outside clippy.toml's allow-expect-in-tests"
)]
fn linked() -> (wasmtime::Engine, Component, Linker<StubHost>) {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, WORLD_SHAPE).expect("the fixture is a valid component");
    let mut linker = Linker::new(&engine);
    Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
        .expect("the generated host traits satisfy every import the world declares");
    (engine, component, linker)
}

/// A context to lend. Nothing in this file reads it — the stub answers every method with a
/// constant — but the representation owns a parsed file, so one has to be parsed.
#[expect(
    clippy::expect_used,
    reason = "a helper in a tests/ crate is outside clippy.toml's allow-expect-in-tests"
)]
fn context() -> CheckContext {
    let source = "const x = 1;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("grammar loads");
    let tree = parser.parse(source, None).expect("parses");
    CheckContext::new(
        NodeArena::new(tree, source.to_owned()),
        FILE,
        std::sync::Arc::new(TypeScript),
    )
}

#[test]
fn a_component_targeting_the_world_instantiates_and_answers_both_probes() {
    let (engine, component, linker) = linked();
    let mut store = Store::new(&engine, StubHost::default());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an out-of-reach deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX / 2);
    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

    assert!(
        rule.call_has_check(&mut store, 0)
            .expect("has-check returns"),
        "the fixture declares a per-file pass"
    );
    assert!(
        rule.call_has_reduce(&mut store, 0)
            .expect("has-reduce returns"),
        "the fixture declares a cross-file pass"
    );
}

/// The per-file phase, end to end: the host owns the context, lends it, and sees the report.
///
/// This is the two-resource shape exercised on the real world rather than on the
/// two-method prototype the decision record measured. What crosses in this one call: a
/// `borrow<check-context>`, a `list<match-entry>` of records carrying strings and handles, a
/// `string` back out through `file-path`, and a `report` with one `option` present and the
/// other absent — the call the TypeScript API could only express as a union second argument.
#[test]
fn the_check_export_receives_a_borrowed_context_and_reports_through_it() {
    let (engine, component, linker) = linked();
    let mut store = Store::new(&engine, StubHost::default());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an out-of-reach deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX / 2);
    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

    let ctx = store
        .data_mut()
        .table
        .push(context())
        .expect("the resource table accepts a context");

    let captures = vec![
        types::MatchEntry {
            name: "callee".to_owned(),
            node: 7,
        },
        types::MatchEntry {
            name: "arg".to_owned(),
            node: 9,
        },
    ];

    rule.call_check(&mut store, 0, Resource::new_borrow(ctx.rep()), &captures)
        .expect("check returns")
        .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported,
        vec![format!("check node=7 message={FILE}: callee,arg fix=none")],
        "the guest read the borrowed context, saw both captures in order, and reported at \
         the first one with a message and no fix"
    );
}

/// The cross-file phase, end to end, over the types only it can reach.
///
/// `reduce-location` declares `option<u32>` positions, so absence is representable and has to
/// survive the canonical ABI. That the guest sends `none` for both and the host receives `None`
/// is the assertion, and that is the whole of it: the stub below records whatever it is handed.
///
/// **The real host refuses this call, and the shape is exercised here anyway.**
/// `lanekeep_wasm::host` fails a report with no line or column — a cross-file violation with no
/// site is unactionable, and 1:1 cannot be told apart from a rule that meant 1:1. See
/// `wit/world.wit`'s `reduce-location` and `tests/reduce.rs`'s
/// `reporting_without_a_position_fails_the_call`. The option is in the record because the
/// published TypeScript `ReduceLocation` has it, not because a positionless report works, so the
/// ABI has to carry a case no rule may rely on.
#[test]
fn the_reduce_export_receives_its_own_context_and_reports_a_partial_location() {
    let (engine, component, linker) = linked();
    let mut store = Store::new(&engine, StubHost::default());

    // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
    // deadline zero — which has always elapsed. Without this line every call into a guest
    // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
    // the epoch in this file, so an out-of-reach deadline is what "no limit" means here;
    // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
    store.set_epoch_deadline(u64::MAX / 2);
    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

    // Empty, and nothing reads it: this file's host answers every reduce method with a
    // constant. What has to exist is a value to lend, because a `borrow<reduce-context>` needs
    // something in the table to borrow from. `tests/reduce.rs` is where a populated one is
    // driven through the real host.
    let ctx = store
        .data_mut()
        .table
        .push(ReduceContext::new(Vec::new(), Vec::new()))
        .expect("the resource table accepts a context");

    rule.call_reduce(&mut store, 0, Resource::new_borrow(ctx.rep()))
        .expect("reduce returns")
        .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported,
        vec!["reduce file=a.ts line=None message=2 files, 1 facts".to_owned()],
        "the guest read the file list and the fact list and reported at a file with no line"
    );
}

/// The load-time import check, and the two ways of counting that disagree.
///
/// `wasm-tools component wit` shows one import on this artifact,
/// `lanekeep:host/types@0.1.0` — the number the decision record's "an import count of
/// exactly one, the declared world" refers to. wasmtime's own view of the same bytes lists
/// **three**: that instance, plus bare `check-context` and `reduce-context` *resource type*
/// imports, which the component model requires because those types appear in the signature
/// of an export. A load check written as `imports.len() == 1` against this API therefore
/// rejects every component that takes a context — which is every rule. The instance imports
/// are the ones that describe reachable capability, and they are what has to be checked.
///
/// One instance rather than three is also the measurement that settled one interface against
/// three: a three-interface split leaves both context types nameable from both phases and
/// changes nothing except this number.
///
/// The subset claim asserted, not just described: what the instance carries is far less than
/// what the world offers.
///
/// This is the second of the two ways a load-time import check can be written wrong, and it
/// is independent of the first. The instance the guest imports declares only the functions it
/// actually calls — three of `check-context`'s twenty-four and three of `reduce-context`'s
/// three — so a check that compares an artifact against `wit/world.wit` for equality rejects
/// every real rule. The test is discriminating rather than decorative: it names the three
/// `check-context` methods the fixture calls and asserts that a method it does not call is
/// absent, which fails the moment either half of the claim stops holding.
#[test]
fn the_imported_instance_declares_only_what_the_guest_calls() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, WORLD_SHAPE).expect("the fixture is a valid component");
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
            "[method]check-context.file-path",
            "[method]check-context.report",
            "[method]check-context.root",
            "[method]reduce-context.facts",
            "[method]reduce-context.files",
            "[method]reduce-context.report",
        ],
        "exactly the six the fixture calls, out of the twenty-seven the world declares"
    );
    assert!(
        !methods.iter().any(|name| name.contains("query-subtree")),
        "a method the fixture never calls is absent from the artifact entirely"
    );
}

/// **What the artifact declares is a subset of what the world offers, and any check written
/// against it has to expect that too.** This guest calls three of `check-context`'s
/// twenty-four methods, and the WIT embedded in its component-type section lists those three
/// and no others; `node-location`, `binding-kind`, `read-error` and `fact-error` do not
/// appear at all. A check comparing the artifact's WIT against the engine's would reject
/// every real rule for a second, independent reason — asserted by
/// [`the_imported_instance_declares_only_what_the_guest_calls`].
#[test]
fn the_component_imports_exactly_the_one_declared_interface() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, WORLD_SHAPE).expect("the fixture is a valid component");

    let ty = component.component_type();

    let all: Vec<&str> = ty.imports(&engine).map(|(name, _)| name).collect();
    assert_eq!(
        all,
        vec![
            "lanekeep:host/types@0.1.0",
            "check-context",
            "reduce-context"
        ],
        "the raw list carries the two resource types the exports name"
    );

    let instances: Vec<&str> = ty
        .imports(&engine)
        .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentInstance(_)))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        instances,
        vec!["lanekeep:host/types@0.1.0"],
        "and exactly one of them is an instance the guest can call into"
    );
}

/// **No fixture artifact imports ambient authority — every one of them, not just this file's.**
///
/// `AGENTS.md` states the rule the hard way round: pin the target at the build *and* check
/// every artifact's import list at load, because neither substitutes for the other. A guest
/// built for `wasm32-wasip1` rather than `wasm32-unknown-unknown` imports `wasi:clocks/
/// wall-clock`, `wasi:filesystem/types` and `wasi:filesystem/preopens` — precisely the
/// capabilities `docs/architecture.md` §13 exists to withhold — and the trap is that this is
/// invisible at small scale: a guest that allocates nothing has zero imports on *both* targets,
/// so a fixture on the wrong target passes every shape assertion right up until a real rule
/// formats a string.
///
/// The two tests above check one artifact out of eleven. Each new fixture widened that gap
/// silently, which is why this one is written the way it is.
///
/// # Globbed, and that is the whole point
///
/// A named list is the mistake this branch has already shipped: two commits before this one, a
/// fix patched three fixture crates by name when there were four, and the fourth was found by
/// a second red CI run rather than by anything here. A directory listing cannot be out of
/// date, so a fixture added by a later task is covered by this the moment it is built — with
/// no step anyone has to remember.
///
/// # A property, not a snapshot
///
/// What is asserted is that every *instance* import is the one host interface. Not a
/// transcript of what the eleven artifacts import today: that would be a list again, and it
/// would have to be edited by whoever adds a fixture, which is the failure mode being fixed.
/// Importing nothing at all passes — `spike.wasm` targets its own `wit/spike.wit` and reaches
/// no part of `std` that touches the adapter — because importing nothing is strictly less
/// authority, never more.
///
/// # The one artifact this deliberately does not see
///
/// `tests/fixtures/rejected/wasip1.wasm` is built for `wasm32-wasip1` on purpose, so that
/// `tests/load.rs` can point the load-time import check at a real wrongly-targeted component
/// rather than a synthetic approximation of one. It imports ten WASI interfaces and would fail
/// every assertion below.
///
/// It is out of scope here by *location* and not by name, which is the same reasoning that
/// made this test a glob: the read below is not recursive, so an artifact in a subdirectory is
/// not a fixture as far as this test is concerned, and there is no exemption list for anyone
/// to add to. An artifact that ever appears beside its siblings is checked, whatever it is
/// called.
#[test]
fn no_fixture_artifact_imports_ambient_authority() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut artifacts: Vec<std::path::PathBuf> = std::fs::read_dir(&directory)
        .expect("the fixtures directory is there")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "wasm"))
        .collect();
    artifacts.sort();

    // A glob that matched nothing would make every assertion below vacuous, which is the one
    // way this test could pass while checking not a single artifact.
    assert!(
        !artifacts.is_empty(),
        "no artifacts under {}: either none are built, or the directory moved and this test \
         has been asserting nothing",
        directory.display()
    );

    let mut observed: Vec<String> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();

    for path in &artifacts {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unnamed>")
            .to_owned();
        let bytes = std::fs::read(path).expect("the artifact is readable");
        let component =
            Component::new(&engine, &bytes).expect("every committed fixture is a valid component");

        // Instances only. wasmtime's raw import list also carries the bare `check-context` and
        // `reduce-context` *resource type* imports the component model requires because those
        // types appear in an export signature — bookkeeping, not reachable capability, and a
        // check written as `imports.len() == 1` would reject every rule over it.
        let ty = component.component_type();
        let mut instances: Vec<&str> = ty
            .imports(&engine)
            .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentInstance(_)))
            .map(|(import, _)| import)
            .collect();
        instances.sort_unstable();

        observed.push(format!(
            "{name}: {}",
            if instances.is_empty() {
                "<no instance imports>".to_owned()
            } else {
                instances.join(", ")
            }
        ));
        offenders.extend(
            instances
                .iter()
                .filter(|import| **import != "lanekeep:host/types@0.1.0")
                .map(|import| format!("{name} imports `{import}`")),
        );
    }

    assert!(
        offenders.is_empty(),
        "a fixture reaches something the world does not declare, which on this evidence means \
         it was built for the wrong target:\n  {}\n\nevery artifact:\n  {}",
        offenders.join("\n  "),
        observed.join("\n  ")
    );
}

/// **And the same check on the components that actually ship — the ones inside the binary.**
///
/// The test above covers `tests/fixtures/`, which is every component this crate builds for its
/// own purposes and none of the ones a user runs. `crates/lanekeep-rules/components/*.wasm` are
/// `include_bytes!`d into `lanekeep_rules::component` and executed against real source on every
/// run, so a wrongly-targeted one is not a red test somewhere — it is ambient authority in a
/// shipped binary. This is the decision record's fourth condition: a load-time import check
/// rather than relying on instantiation to fail by accident.
///
/// # Reusing the engine's own filter rather than a second copy of it
///
/// [`instance_imports`] is what `lanekeep_wasm::load::check_imports` calls before every
/// instantiation, and calling it here means this test and the production path cannot disagree
/// about what an import is. A hand-written `imports().len() == 1` would reject every one of
/// these artifacts, because the component model adds bare `check-context` and `reduce-context`
/// resource-type imports for the types the exports name — the trap the test above documents at
/// length and the reason there is one filter rather than two.
///
/// # Equality here, a subset above
///
/// A fixture importing nothing at all passes the test above, because nothing is strictly less
/// authority than the host interface. A *rule* cannot: a rule reports, and `report` is a host
/// method, so a shipped component whose instance import list is empty is a rule that can never
/// say anything. Asserting equality rather than containment is what makes this fail on a rule
/// that was gutted as well as on one that was over-permitted.
///
/// Globbed, for the reason the test above is: a named list is a step someone has to remember,
/// and rules land in this directory as they are migrated — two are here now, and the glob picked
/// both up without this test being touched, which is the property it was written for.
#[test]
fn no_shipped_rule_component_imports_ambient_authority() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");

    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lanekeep-rules/components");
    let mut artifacts: Vec<std::path::PathBuf> = std::fs::read_dir(&directory)
        .expect("the shipped components directory is there")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "wasm"))
        .collect();
    artifacts.sort();

    // A glob that matched nothing would make every assertion below vacuous, which is the one
    // way this test could pass while checking not a single shipped component.
    assert!(
        !artifacts.is_empty(),
        "no components under {}: either none are built, or the directory moved and this test \
         has been asserting nothing",
        directory.display()
    );

    let mut wrong: Vec<String> = Vec::new();
    for path in &artifacts {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unnamed>")
            .to_owned();
        let bytes = std::fs::read(path).expect("the artifact is readable");
        let component = Component::new(&engine, &bytes)
            .expect("every committed rule component is a valid component");

        let imports = instance_imports(&engine, &component);
        if imports != vec![HOST_INTERFACE.to_owned()] {
            wrong.push(format!("{name} imports {imports:?}"));
        }

        // And the production check itself, on the set the loader defaults to. The equality
        // above is the stronger claim; this one is the claim that matters, because it is the
        // code that runs.
        if let Err(refused) = check_imports(
            &engine,
            &name,
            &component,
            &PermittedImports::declared_world(),
        ) {
            wrong.push(format!("{name} is refused by the loader: {refused}"));
        }
    }

    assert!(
        wrong.is_empty(),
        "a shipped rule component does not import exactly `{HOST_INTERFACE}`, which on this \
         evidence means it was built for the wrong target — and unlike a fixture, this one is \
         inside the binary:\n  {}",
        wrong.join("\n  ")
    );
}
