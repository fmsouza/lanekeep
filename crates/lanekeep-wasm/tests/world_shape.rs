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

/// The TypeScript built-ins, as built by `just typescript-builtins`.
///
/// Named here rather than left to the glob below so that its *absence* is a compile error —
/// see [`the_typescript_builtins_component_imports_no_ambient_authority`], which is the whole
/// reason this constant exists.
const TYPESCRIPT_BUILTINS: &[u8] =
    include_bytes!("../../lanekeep-rules/components/typescript-builtins.wasm");

/// The Go built-ins, as built by `just go-rules`.
///
/// Named for the reason [`TYPESCRIPT_BUILTINS`] is, and the reason bites harder here: `just
/// go-rules` needs TinyGo, which no gate installs and most contributors do not have, so a
/// checkout where nobody has run it would leave [`no_shipped_rule_component_imports_ambient_authority`]
/// green over the components beside this one and silent about its absence. `include_bytes!` is
/// not silent.
const GO_BUILTINS: &[u8] = include_bytes!("../../lanekeep-rules/components/go-builtins.wasm");

/// The path the stub reports as the file under check.
const FILE: &str = "src/lib.ts";

/// The text the stub reports for node 0, and for no other node.
///
/// `init`, because [`the_go_builtins_component_answers_the_world_it_targets`] drives a guest that
/// branches on exactly this string: `lanekeep/no-package-init` reports a `func init()` and
/// nothing else.
const INIT_TEXT: &str = "init";

/// The text the stub reports for every node the table below does not name.
///
/// **Several constants rather than one, and the others are what make this one mean something.** A
/// stub answering `init` for every node cannot tell a rule that fires on `func init()` from a
/// rule that fires on every function declaration it is handed — both report, and reporting is
/// what a working rule looks like. Keyed on the handle, one test can drive a rule several times
/// and watch it decline all but once.
const OTHER_TEXT: &str = "Register";

/// The handles the stub answers specially, and what each one stands for.
///
/// A handle is meaningless on its own — the arena the real host reads is not here — so these are
/// simply the addresses at which this stub keeps its answers. What each one is *for* is the
/// branch of a guest it lets a test reach:
///
/// | handle | `text` | `binding-kind` | the branch it exercises |
/// | --- | --- | --- | --- |
/// | 0 | `init` | const | `no-package-init` reports |
/// | 2 | `Context` | const | `no-context-in-struct`'s type name matches |
/// | 3 | `context` | **import** | its qualifier is an imported package: reports |
/// | 4 | `context` | const | a local name that reads `context`: declines |
/// | 5 | `context` | **none** | a qualifier that resolves to nothing: declines |
/// | any other | `Register` | const | neither rule fires |
///
/// Handle 5 is not symmetry. `binding-kind` is an `option<binding-kind>`, and `import` is the
/// **first** case of that enum — so a guest reading the option's value without first asking
/// whether it has one gets `import` for a name that resolves to no binding at all, and reports a
/// type that merely reads like the standard library's. That is the same shape as the `if (!node)`
/// bug that cost `no-unwrap` its `#[test]` exemption, and it is invisible without a `none` here.
const NODE_INIT: u32 = 0;
const NODE_CONTEXT_TYPE: u32 = 2;
const NODE_IMPORTED_PKG: u32 = 3;
const NODE_LOCAL_PKG: u32 = 4;
const NODE_UNRESOLVED_PKG: u32 = 5;

/// The text a `context.Context` field's type name carries.
const CONTEXT_TYPE_TEXT: &str = "Context";

/// The text its qualifier carries, whatever the qualifier turns out to resolve to.
const CONTEXT_PKG_TEXT: &str = "context";

/// Where each Go built-in sits in `go-rules/main.go`'s table.
///
/// **Alphabetical, and that is a constraint rather than a habit.** `crates/lanekeep-rules`'
/// `COMPONENT_RULES` is sorted by rule name, and the index in each of its rows is what this
/// component is then dispatched on — so a table in a different order here means a config naming
/// one rule running the other, with both answering perfectly well and nothing to notice. Adding a
/// third Go rule renumbers whichever of these it sorts before.
const NO_CONTEXT_IN_STRUCT: u32 = 0;
const NO_PACKAGE_INIT: u32 = 1;

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

    /// A constant per handle — still constants, and still nothing read out of the context, but
    /// several of them, so a guest that branches on the text can be watched taking each branch.
    /// The table is in [`NODE_INIT`]'s doc comment.
    ///
    /// The fixture this file was written for never calls `text` at all — see
    /// [`the_imported_instance_declares_only_what_the_guest_calls`], which names the three
    /// `check-context` methods it does call — so nothing but the two tests over the shipped Go
    /// component can observe any of them.
    fn text(&mut self, _: Resource<CheckContext>, node: u32) -> wasmtime::Result<Option<String>> {
        Ok(Some(
            match node {
                NODE_INIT => INIT_TEXT,
                NODE_CONTEXT_TYPE => CONTEXT_TYPE_TEXT,
                NODE_IMPORTED_PKG | NODE_LOCAL_PKG | NODE_UNRESOLVED_PKG => CONTEXT_PKG_TEXT,
                _ => OTHER_TEXT,
            }
            .to_owned(),
        ))
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

    /// One handle resolves to an import, one to nothing at all, and everything else to a `const`.
    ///
    /// The `None` arm is the one worth having: it is the answer for a name that resolves to no
    /// binding, and it is what tells a guest that reads this option's *value* from one that asks
    /// whether it has one. See [`NODE_INIT`]'s table.
    fn binding_kind(
        &mut self,
        _: Resource<CheckContext>,
        node: u32,
    ) -> wasmtime::Result<Option<BindingKind>> {
        Ok(match node {
            NODE_IMPORTED_PKG => Some(BindingKind::Import),
            NODE_UNRESOLVED_PKG => None,
            _ => Some(BindingKind::Const),
        })
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
/// No `expect` grant of its own, because it makes no `expect` call: an unfulfilled `#[expect]`
/// is itself an error under this workspace's lints, so the attribute has to sit where the calls
/// actually are, which is [`linked_to`].
fn linked() -> (wasmtime::Engine, Component, Linker<StubHost>) {
    linked_to(WORLD_SHAPE)
}

/// The same, over any component targeting this world.
///
/// The parameter is what lets a *shipped* component be driven through the stub host rather than
/// only inspected — see [`the_go_builtins_component_answers_the_world_it_targets`]. Every host
/// method here answers with a constant, so what this proves is that the guest half of the world
/// is satisfied, which is precisely the claim this file is about.
///
/// A crate-level `expect` is what licenses the `expect` calls here. `clippy.toml`'s
/// `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]` modules; a helper
/// in an integration-test crate is neither, so the identical line that passes inside a test
/// body fails the gate out here.
#[expect(
    clippy::expect_used,
    reason = "a helper in a tests/ crate is outside clippy.toml's allow-expect-in-tests"
)]
fn linked_to(bytes: &[u8]) -> (wasmtime::Engine, Component, Linker<StubHost>) {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, bytes).expect("the component is valid");
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
/// The two tests above check one artifact; this directory holds fifteen of them, and it held
/// eleven when that sentence was first written. Each new fixture widened the gap silently, which
/// is why this one is written the way it is — and why the count above is the only number here,
/// stated as something that moves rather than as a fact about the tree. Take it from `ls
/// crates/lanekeep-wasm/tests/fixtures/*.wasm` rather than from this sentence; the test itself
/// never reads it.
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
/// transcript of what the artifacts in this directory import today: that would be a list again,
/// and it would have to be edited by whoever adds a fixture, which is the failure mode being
/// fixed.
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

/// **The one shipped component that is built from JavaScript, named rather than globbed.**
///
/// [`no_shipped_rule_component_imports_ambient_authority`] below already makes this claim about
/// every artifact in that directory, and it makes it the better way — a glob cannot go stale. So
/// this test exists for the one thing a glob cannot do: **a directory listing is satisfied by a
/// directory that does not contain the artifact at all.** `include_bytes!` is not. Delete
/// `typescript-builtins.wasm`, or never build it, and this crate does not compile, naming the
/// path; the glob goes green over the two Rust components beside it and says nothing.
///
/// That is not hypothetical here. This artifact is the only one in the tree produced by a recipe
/// that needs Node, and `just typescript-builtins` is outside every gate for the reason
/// `just rust-rules` is. A contributor who has never run it has a checkout where the glob is
/// still green.
///
/// # Equality, not containment
///
/// `{"lanekeep:host/types@0.1.0"}` exactly. A component built without `--disable all` imports
/// `wasi:filesystem/{types,preopens}`, `wasi:clocks/{wall-clock,monotonic-clock}`,
/// `wasi:random/random` and `wasi:http/{types,outgoing-handler}` — filesystem, clock, randomness
/// and network, in a component whose whole purpose is to have none of them — and a containment
/// check would pass an artifact that imports the host interface *and* all seven. The other
/// direction matters too: a component that imports nothing is a rule that cannot call `report`,
/// and it is what a bundle that silently dropped the runtime looks like.
#[test]
fn the_typescript_builtins_component_imports_no_ambient_authority() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, TYPESCRIPT_BUILTINS)
        .expect("the shipped TypeScript built-ins are a valid component");

    let imports = instance_imports(&engine, &component);
    assert_eq!(
        imports,
        vec![HOST_INTERFACE.to_owned()],
        "equality, not containment: a component importing nothing is also wrong"
    );
}

/// **And the one shipped component built from Go, on exactly the same terms.**
///
/// The equality is what makes this worth stating separately from the glob below, because TinyGo
/// is the toolchain in this tree with the most to give away by accident. Its `wasip2` target
/// imports `wasi:clocks/wall-clock`, `wasi:filesystem/types`, `wasi:cli/stdout` and
/// `wasi:random/random` unconditionally — the guest's runtime reaches for them whether or not any
/// rule does — so a rebuild on it is not a rule that leaks a little, it is a rule with a clock, a
/// filesystem and a source of randomness in a sandbox whose whole purpose is to withhold all
/// three. `-target=wasm-unknown` is what keeps the list at one, and this is the assertion that
/// says so.
///
/// The other direction matters here too, and is not symmetry: a component that imports nothing at
/// all is a rule that cannot call `report`, which is what a guest built with its host import
/// tree-shaken away would look like.
#[test]
fn the_go_builtins_component_imports_no_ambient_authority() {
    let engine = engine().expect("the shipped wasmtime configuration builds an engine");
    let component = Component::new(&engine, GO_BUILTINS)
        .expect("the shipped Go built-ins are a valid component");

    let imports = instance_imports(&engine, &component);
    assert_eq!(
        imports,
        vec![HOST_INTERFACE.to_owned()],
        "equality, not containment: a component importing nothing is also wrong"
    );
}

/// And the Go component answers the world, rather than merely satisfying its type.
///
/// A digest says the artifact was built from the sources beside it; the test above says it reaches
/// nothing it should not. Neither says the thing was *built right* — a guest whose exports were
/// wired to the wrong handlers, or whose rule table came out empty, passes both. `rules` is the
/// export that settles it: it is what a host enumerates before anything else, and what
/// `crates/lanekeep-rules`' own dispatch table is checked against once this component ships.
///
/// **Here as well as in `crates/lanekeep-rules/tests/component_rules.rs`**, which is where the
/// equivalent claim about the other three components lives. That file reads its components out of
/// `lanekeep_rules::component`, which resolves a *rule name*, and it now names both Go rules — so
/// the two tests overlap, deliberately. This one is the lower of the pair: it links the component
/// against the world in `crates/lanekeep-wasm/wit/` directly, so it fails on a component that no
/// longer answers the world it was built for, where the other fails on a *table* that disagrees
/// with what the component enumerates. A rebuild against a moved world reddens this file first,
/// and it says so in terms of the ABI rather than in terms of a rule name.
///
/// It was written when the swap had not happened and no rule name resolved here at all, which is
/// the state that made it the only place this component could be reached. That is no longer the
/// reason it exists; the paragraph above is.
#[test]
fn the_go_builtins_component_answers_the_world_it_targets() {
    let (engine, component, linker) = linked_to(GO_BUILTINS);
    let mut store = Store::new(&engine, StubHost::default());
    // Out of reach, for the reason `a_component_targeting_the_world_instantiates_and_answers_both_probes`
    // gives: `lanekeep_wasm::engine` enables epoch interruption and every store starts at a
    // deadline that has already elapsed.
    store.set_epoch_deadline(u64::MAX / 2);

    let rule = Rule::instantiate(&mut store, &component, &linker)
        .expect("the Go component instantiates against this world");

    assert_eq!(
        rule.call_rules(&mut store).expect("rules returns"),
        vec![
            "lanekeep/no-context-in-struct".to_owned(),
            "lanekeep/no-package-init".to_owned(),
        ],
        "the ids `go-rules/main.go`'s table declares, in the order it declares them — which is \
         the order every other export's index is read against, and which is alphabetical because \
         `crates/lanekeep-rules`' own tables are"
    );
    assert!(
        rule.call_has_check(&mut store, NO_PACKAGE_INIT)
            .expect("has-check returns"),
        "the rule at this index has a per-file pass"
    );
    assert!(
        !rule
            .call_has_reduce(&mut store, NO_PACKAGE_INIT)
            .expect("has-reduce returns"),
        "and no cross-file one: every export is mandatory because a WIT world has no optional \
         ones, which is not the same as every pass being present"
    );

    // What the rule says it is. The prose of the card is held to the TypeScript rule it was
    // ported from by the shared case table those two run against; what is asserted here is the
    // shape, and specifically the three gate lists that are **empty**. An empty `cm.List` in a
    // Go guest is a null data pointer with a zero length, and it crosses the canonical ABI at
    // every one of these fields — so if that lifted wrong, it would do so in `metadata`, once,
    // before any file is read, and take the whole run with it.
    let metadata = rule
        .call_metadata(&mut store, NO_PACKAGE_INIT)
        .expect("metadata returns");
    assert_eq!(metadata.id, "lanekeep/no-package-init");
    assert_eq!(metadata.languages, vec!["go".to_owned()]);
    assert_eq!(metadata.gates.file_contains, vec!["init".to_owned()]);
    assert!(
        metadata.gates.path_matches.is_empty()
            && metadata.gates.path_not_matches.is_empty()
            && metadata.gates.file_not_contains.is_empty(),
        "the three gates this rule does not use are empty lists rather than anything else: \
         {:?}",
        metadata.gates
    );

    // `null` is what the world sends a rule named with no options, and every rule is configured
    // once before any check. This one has none and accepts anyway; see `takesNoOptions`.
    rule.call_configure(&mut store, NO_PACKAGE_INIT, "null")
        .expect("configure returns")
        .expect("a rule with no options accepts the world's `null`");

    // The per-file pass, end to end. The stub answers `text` with [`INIT_TEXT`] at node 0, so a
    // match whose `name` capture binds node 0 is a `func init()` as far as the guest can tell.
    //
    // **This is the call that a component built the obvious way does not survive**, and nothing
    // cheaper than making it finds out. `check` takes a `borrow<check-context>`, the canonical
    // ABI counts that loan, and `wit-bindgen-go` emits no `resource.drop` to close it — so a
    // guest that simply used the handle returns with the loan outstanding and wasmtime answers
    // `borrow handles still remain at the end of the call`, discarding every report the rule
    // made on the way. `go-rules/main.go` drops it; this is what says so.
    let check_ctx = store
        .data_mut()
        .table
        .push(context())
        .expect("the resource table accepts a check context");
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(&mut store, NO_PACKAGE_INIT, borrow, &captures(NODE_INIT))
        .expect("check returns")
        .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported,
        vec![
            "check node=7 message=`init` runs at import time in an order nothing states, so what \
             it sets up is untraceable from the code that depends on it fix=none"
                .to_owned()
        ],
        "the rule reported at the node its `func` capture bound, not at the one its `name` \
         capture did"
    );

    // And the same call over a declaration named something else, which must report nothing.
    //
    // Without this the test cannot tell this rule from one that fires on every function
    // declaration handed to it — both report, and reporting is what a working rule looks like.
    // The only thing that differs between the two calls is which node the `name` capture binds,
    // and so which of the stub's two texts the guest reads.
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(&mut store, NO_PACKAGE_INIT, borrow, &captures(1))
        .expect("check returns")
        .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported.len(),
        1,
        "a declaration whose name is not `init` is not a violation: {:?}",
        store.data().reported
    );

    // And the cross-file pass a rule without one refuses, through the error channel rather than
    // by trapping. `frames` is empty, which is the other zero-length list crossing the ABI.
    let reduce_ctx = store
        .data_mut()
        .table
        .push(ReduceContext::new(Vec::new(), Vec::new()))
        .expect("the resource table accepts a reduce context");
    let refused = rule
        .call_reduce(
            &mut store,
            NO_PACKAGE_INIT,
            Resource::new_borrow(reduce_ctx.rep()),
        )
        .expect("reduce returns")
        .expect_err("this rule has no cross-file pass");
    assert!(
        refused.message.contains("no cross-file pass"),
        "{refused:?}"
    );
    assert!(refused.frames.is_empty(), "{refused:?}");
}

/// One match of `(function_declaration name: (identifier) @name) @func`, with `name` at `node`.
///
/// The declaration is always node 7, so what varies between calls is only the text the stub
/// answers for the name — which is the one input the rule under test branches on.
fn captures(node: u32) -> Vec<types::MatchEntry> {
    vec![
        types::MatchEntry {
            name: "name".to_owned(),
            node,
        },
        types::MatchEntry {
            name: "func".to_owned(),
            node: 7,
        },
    ]
}

/// And the second rule the same component hosts, driven the same way.
///
/// A test of its own rather than more assertions in the one above, because what makes the pair
/// worth having is that **both are reached through the same instance by index**: the rule that
/// answers is decided by a number, and a component whose table drifted out of alphabetical order
/// would answer every call perfectly while answering for the wrong rule. `rules` is asserted
/// there; this is the other end of the same claim, that index 0 is the rule `rules` named at
/// position 0.
///
/// What the guest reads here is three host answers per match — the type name's text, the
/// qualifier's text, and the qualifier's binding kind — so the four calls below walk the rule's
/// four outcomes. The `none` binding kind is the one that would be missed by eye; see
/// [`NODE_INIT`]'s table for why an option whose first enum case is `import` is a trap rather
/// than a formality.
///
/// **This is not the fidelity suite.** Every value here is a constant and no Go file is parsed;
/// `crates/lanekeep-rules/tests/no_context_in_struct.rs` is where the rule meets the real grammar
/// and the real resolver. What this can say, and that file cannot until the built-in is swapped
/// over, is that the shipped artifact hosts a working second rule at all.
#[test]
fn the_go_builtins_component_runs_its_second_rule_by_index() {
    let (engine, component, linker) = linked_to(GO_BUILTINS);
    let mut store = Store::new(&engine, StubHost::default());
    store.set_epoch_deadline(u64::MAX / 2);
    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

    let metadata = rule
        .call_metadata(&mut store, NO_CONTEXT_IN_STRUCT)
        .expect("metadata returns");
    assert_eq!(metadata.id, "lanekeep/no-context-in-struct");
    assert_eq!(metadata.languages, vec!["go".to_owned()]);
    assert_eq!(metadata.gates.file_contains, vec!["context".to_owned()]);
    assert!(
        rule.call_has_check(&mut store, NO_CONTEXT_IN_STRUCT)
            .expect("has-check returns")
            && !rule
                .call_has_reduce(&mut store, NO_CONTEXT_IN_STRUCT)
                .expect("has-reduce returns"),
        "a per-file rule, like its neighbor"
    );

    let check_ctx = store
        .data_mut()
        .table
        .push(context())
        .expect("the resource table accepts a check context");

    // A `context.Context` field whose qualifier is an imported package: the one case that reports.
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(
        &mut store,
        NO_CONTEXT_IN_STRUCT,
        borrow,
        &field(NODE_IMPORTED_PKG, NODE_CONTEXT_TYPE),
    )
    .expect("check returns")
    .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported,
        vec![
            "check node=9 message=a context.Context stored in a struct outlives the call it was \
             scoped to, so cancelling one request can cancel unrelated work fix=none"
                .to_owned()
        ],
        "reported at the node the `field` capture bound, not at either of the two it read"
    );

    // The same field, with a qualifier that is a local `const` rather than an import. A rule that
    // skipped the binding kind reports here, and the report would look exactly like the one above.
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(
        &mut store,
        NO_CONTEXT_IN_STRUCT,
        borrow,
        &field(NODE_LOCAL_PKG, NODE_CONTEXT_TYPE),
    )
    .expect("check returns")
    .expect("the guest does not report a failure");

    // And with a qualifier that resolves to nothing at all. `import` is the first case of
    // `binding-kind`, so a guest reading the option's value without asking whether it has one
    // gets `import` here and reports — silently, and only ever by adding violations.
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(
        &mut store,
        NO_CONTEXT_IN_STRUCT,
        borrow,
        &field(NODE_UNRESOLVED_PKG, NODE_CONTEXT_TYPE),
    )
    .expect("check returns")
    .expect("the guest does not report a failure");

    // And an imported qualifier carrying some other type entirely.
    let borrow = Resource::new_borrow(check_ctx.rep());
    rule.call_check(
        &mut store,
        NO_CONTEXT_IN_STRUCT,
        borrow,
        &field(NODE_IMPORTED_PKG, 6),
    )
    .expect("check returns")
    .expect("the guest does not report a failure");

    assert_eq!(
        store.data().reported.len(),
        1,
        "only the imported `context.Context` is a violation: {:?}",
        store.data().reported
    );
}

/// One match of this rule's query, with the qualifier at `pkg` and the type name at `name`.
///
/// The field declaration is always node 9, so what varies between calls is only which of the
/// stub's answers the guest reads for the two nodes it inspects.
///
/// Both of the rule's query patterns bind exactly these three captures — that is what lets the
/// bare and pointer forms share one handler — so one shape here covers both.
fn field(pkg: u32, name: u32) -> Vec<types::MatchEntry> {
    vec![
        types::MatchEntry {
            name: "pkg".to_owned(),
            node: pkg,
        },
        types::MatchEntry {
            name: "name".to_owned(),
            node: name,
        },
        types::MatchEntry {
            name: "field".to_owned(),
            node: 9,
        },
    ]
}

/// An index the component does not host is refused, and the two ways it is refused differ.
///
/// `configure`, `check` and `reduce` return a `result`, so they answer with a message naming what
/// went wrong. `metadata`, `has-check` and `has-reduce` have no channel at all, so the only
/// honest answer left is a trap: inventing one rule's answer for another rule's index would be a
/// host and a component silently disagreeing about which rule is running, which is the failure
/// the `rules`-then-index arrangement exists to make impossible.
///
/// **A store of its own, and that is not tidiness.** A trap poisons a `wasmtime::Store`
/// permanently — every later call on it fails with `cannot enter component instance`, a message
/// about the runtime's bookkeeping that names nothing that went wrong — so a trap probe sharing a
/// store with anything else would make whatever ran after it fail for a reason that has nothing
/// to do with the code under test. The graceful refusal is asserted first here for the same
/// reason: after the trap there is nothing left to ask.
#[test]
fn the_go_builtins_component_refuses_an_index_it_does_not_host() {
    let (engine, component, linker) = linked_to(GO_BUILTINS);
    let mut store = Store::new(&engine, StubHost::default());
    store.set_epoch_deadline(u64::MAX / 2);
    let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

    let refused = rule
        .call_configure(&mut store, 7, "null")
        .expect("configure returns")
        .expect_err("this component hosts no rule at index 7");
    assert!(
        refused.contains("no rule at index 7"),
        "the refusal has to name the index it refused: {refused}"
    );

    // `root_cause`, not the rendered error, on this crate's standing terms: wasmtime prefixes a
    // failure with a backtrace whose frames already name the export, so a `to_string` containing
    // `has-check` would pass whatever happened — and here the rendered form does not carry the
    // trap's own words at all, only its `Caused by:` does.
    let trapped = rule
        .call_has_check(&mut store, 7)
        .expect_err("an export with no error channel can only trap");
    assert_eq!(
        trapped.root_cause().to_string(),
        "wasm trap: wasm `unreachable` instruction executed",
        "a guest with nowhere to put a refusal traps rather than answering: {trapped:?}"
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
