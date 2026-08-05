//! The host side of `lanekeep:host@0.1.0`: what a rule component can actually reach.
//!
//! This is the trust boundary made concrete, the same job
//! `crates/lanekeep-js/src/host.rs` does for QuickJS. The difference is where the boundary
//! comes from. Under QuickJS a rule reaches whatever was *installed* on `ctx`, so the surface
//! is curated and its absences are maintained; here a component reaches exactly what its
//! world declares, so the surface is [`wit/world.wit`](../../wit/world.wit) and its absences
//! are structural. Adding a function there widens the trust boundary and bumps the host API
//! version that feeds the cache key. Adding one here implements what is already declared.
//!
//! # What is implemented so far
//!
//! `check-context`'s navigation, reporting and binding resolution. Everything else the world
//! declares — scoped queries, tracked reads, facts, `today`, and the whole of
//! `reduce-context` — returns an error naming itself, which traps the call.
//!
//! **Trapping rather than answering, deliberately.** Every plausible placeholder is a
//! plausible *answer*: an empty list from `query-subtree` reads as "nothing matched", `none`
//! from `read-file` reads as "the file is not there". A rule built against either would look
//! like it was working, and the run would be wrong quietly. An error is the one response no
//! caller can mistake for a result.
//!
//! **Binding resolution is not one of those, and the difference is not a relaxation.** `false`
//! and `none` are what "nothing resolves" *is*: a name nothing declares, a handle no arena
//! issued, and a language whose [`lanekeep_lang::Language::resolver`] returns `None` are three
//! ways of having no binding, and a rule acts on all three identically. The unimplemented
//! methods trap because they have no answer to give; these have one.
//!
//! # Two engines, one arena, one answer
//!
//! Every navigation method is a thin call into [`NodeArena`], which is also what
//! `lanekeep-js` calls. That is not code reuse for its own sake: two engines that disagreed
//! about a node's parent or identity for one file would let a single run disagree with
//! itself, and the shared type is what stops the question from being askable. Where the WIT
//! shape forces a difference from the JavaScript surface, it is named at the method.
//!
//! Binding resolution needed one layer more. The arena resolves an identifier to a
//! [`Binding`], but *which module and export count as a match* — and what a `*` in a pattern
//! does — was written out in `lanekeep-js` and would have been written out a second time here.
//! It now lives on [`Binding`] itself, in `lanekeep-lang`, and both engines call it. A copy in
//! each would let one file resolve differently depending on which engine ran the rule, and
//! nothing would fail: both would answer, plausibly, and disagree.
//!
//! # A rule always has all four, whatever its language
//!
//! A component imports what its world declares, so `resolves-to-import`, `is-imported-from`,
//! `binding-kind` and `is-shadowed` are present for every rule. A resource's method set is
//! fixed by the world, so making a method absent for one file and present for another is not
//! something the component model can express at all: it would take a distinct resource type
//! per combination of capabilities a host might attach, and a world naming all of them.
//!
//! This is not a departure from QuickJS, which is worth stating because it looks like one:
//! `lanekeep-js` installs these four unconditionally too, closing over an `Option` and
//! answering `false`/`undefined` without a resolver — see `HostContext::with_resolver`. The
//! surfaces that *are* absent there rather than answering are the ones attached per run and
//! per language for other reasons: `querySubtree` and `closestAncestor` without a grammar,
//! `readFile` and `fileExists` without file access, `today` without a date. Those are the
//! places where a later change here has a real decision to make, and it is theirs to make.
//!
//! # No interior mutability, and why that is not an oversight
//!
//! `lanekeep-js` holds its arena as `Rc<RefCell<NodeArena>>` because rquickjs requires
//! `'static` closures, so every host function must own a shared handle to the state it reads.
//! Nothing of the sort applies here: a host method receives `&mut self` and looks the context
//! up in the store's [`ResourceTable`], so it already holds the unique borrow the arena's
//! interning methods want. The plain `NodeArena` below is the same type used the simpler way,
//! not a second design.

use std::sync::Arc;

use lanekeep_core::fix::Fix;
use lanekeep_lang::binding::{Binding, BindingKind as LangBindingKind, BindingResolver};
use lanekeep_nodes::{Handle, NodeArena};
use wasmtime::component::{Resource, ResourceTable};

use crate::bindings::types::{
    self, BindingKind, EmittedFact, FactError, Host, HostCheckContext, HostReduceContext,
    NodeLocation, ReadError, ReduceContext, ReduceLocation,
};

/// A violation a rule asked for.
///
/// Deliberately not a [`lanekeep_core::Violation`]. A rule supplies a position and optionally
/// a message; the rule's identity, severity and card come from the engine, which is what
/// stops a rule from reporting under someone else's name.
///
/// It carries the same five fields as `lanekeep_js::Report` and is a separate type, because
/// nothing should make `lanekeep-wasm` depend on the engine it is eventually replacing. The
/// one field that differs says something true: `node` is a [`Handle`] rather than an
/// `Option<Handle>`, since WIT's `report(n: node, ...)` has no form that omits the node,
/// where the JavaScript signature's `Option` was never `None` in practice either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The node reported at.
    pub node: Handle,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// A message overriding the rule card's, when the rule supplied one.
    pub message: Option<String>,
    /// A replacement the rule offered.
    pub fix: Option<Fix>,
}

/// Host state for one file: the tree a rule is reading and what it reported about it.
///
/// This is the representation `wit/world.wit`'s `check-context` resource is stored as — the
/// type named in [`crate::bindings`]'s `with` mapping. The engine owns one of these, pushes
/// it into the store's [`ResourceTable`], and lends the guest a `borrow` of it for the length
/// of one `check` call.
///
/// `Debug` is hand-written for the reason `lanekeep_js::HostContext`'s is: requiring it on
/// [`BindingResolver`] would burden every language implementation for the sake of one derive.
pub struct CheckContext {
    arena: NodeArena,
    file_path: String,
    reports: Vec<Report>,
    resolver: Option<Arc<dyn BindingResolver>>,
}

impl std::fmt::Debug for CheckContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckContext")
            .field("file_path", &self.file_path)
            .field("interned_nodes", &self.arena.len())
            .field("reports", &self.reports.len())
            .field("has_resolver", &self.resolver.is_some())
            .finish()
    }
}

impl CheckContext {
    /// Build a context over a parsed file.
    ///
    /// Takes an arena rather than a `tree_sitter::Tree` and a source, which is the one place
    /// this differs from `lanekeep_js::HostContext::new`. Both build the identical
    /// [`NodeArena`]; taking it already built keeps `tree-sitter` out of this crate's public
    /// API, and the caller that parses the file is holding one anyway.
    #[must_use]
    pub fn new(arena: NodeArena, file_path: &str) -> Self {
        Self {
            arena,
            file_path: file_path.to_owned(),
            reports: Vec::new(),
            resolver: None,
        }
    }

    /// Attach a binding resolver, so the four binding-resolution methods have something to
    /// ask.
    ///
    /// Without one they answer `false` and `none` rather than trapping — see this module's
    /// header for why that is an answer here and not a placeholder. The same posture
    /// `lanekeep_js::HostContext::with_resolver` documents, and for the same reason: a rule
    /// written against a language with no resolver should behave as though nothing resolves.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn BindingResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// What the identifier at a handle refers to, or `None` when nothing does.
    ///
    /// The one place the two questions "is there a resolver" and "does this name resolve"
    /// are collapsed, because every caller below treats them identically. A language with no
    /// resolver and a name nothing declares are both "no binding", which is the answer the
    /// world's `bool` and `option<binding-kind>` returns can carry.
    fn binding(&self, n: Handle) -> Option<Binding> {
        self.arena.resolve_binding(n, self.resolver.as_deref()?)
    }

    /// The tree this context is reading.
    #[must_use]
    pub const fn arena(&self) -> &NodeArena {
        &self.arena
    }

    /// The tree, for interning query captures before invoking a handler.
    ///
    /// Mutable because interning is what issues a handle, and the caller building a `match`
    /// to pass to `check` has to do that before the call rather than during it.
    pub const fn arena_mut(&mut self) -> &mut NodeArena {
        &mut self.arena
    }

    /// The path of the file being checked, relative to the project root.
    #[must_use]
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Take everything reported so far, leaving the context empty.
    ///
    /// Emptying rather than copying, so a context read twice cannot report a violation twice.
    #[must_use]
    pub fn take_reports(&mut self) -> Vec<Report> {
        std::mem::take(&mut self.reports)
    }
}

/// The store's data: the table the lent contexts live in.
///
/// One per [`wasmtime::Store`]. Every host method below receives this as `&mut self` together
/// with a [`Resource`] naming which context the call was made through, which is what allows
/// one store to hold several without a method ever having to guess which is current.
#[derive(Debug, Default)]
pub struct HostState {
    table: ResourceTable,
}

impl HostState {
    /// An empty state, holding no contexts.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a context in the table, so it can be lent to a guest.
    ///
    /// # Errors
    ///
    /// Returns the table's error when it cannot accept another entry.
    pub fn push_check_context(
        &mut self,
        context: CheckContext,
    ) -> wasmtime::Result<Resource<CheckContext>> {
        Ok(self.table.push(context)?)
    }

    /// The context a resource names, for the caller that owns it.
    ///
    /// # Errors
    ///
    /// Returns the table's error when the resource is not live — which for a call arriving
    /// from a guest means a handle the component model should never have let through, and
    /// for a call from the engine means the context was already dropped.
    pub fn check_context_mut(
        &mut self,
        handle: &Resource<CheckContext>,
    ) -> wasmtime::Result<&mut CheckContext> {
        Ok(self.table.get_mut(handle)?)
    }
}

/// How a binding is spelled at the boundary.
///
/// The world's `binding-kind` is an enum where `lanekeep-js` hands JavaScript a string, so
/// this is the one place the two engines' *representations* differ rather than their answers.
/// Two things keep them from drifting apart. The match is exhaustive over
/// [`lanekeep_lang::binding::BindingKind`], so a kind added there stops this crate from
/// building rather than quietly becoming something else. And the case names correspond
/// one-for-one with what [`Binding::kind_str`] returns and with the `BindingKind` union in
/// `packages/lanekeep/index.d.ts`, which is what a rule author reads — `catch-param` here is
/// `'catch-param'` there, and both are the same binding.
///
/// An enum rather than a string is the boundary's own improvement on that surface: a rule that
/// compares a kind against a misspelled string literal under QuickJS gets `false` and no error,
/// and the same mistake in a component does not compile.
const fn wit_kind(binding: &Binding) -> BindingKind {
    match binding {
        Binding::Import { .. } => BindingKind::Import,
        Binding::Local(kind) => match *kind {
            LangBindingKind::Const => BindingKind::Const,
            LangBindingKind::Let => BindingKind::Let,
            LangBindingKind::Var => BindingKind::Var,
            LangBindingKind::Param => BindingKind::Param,
            LangBindingKind::Function => BindingKind::Function,
            LangBindingKind::Class => BindingKind::Class,
            LangBindingKind::CatchParam => BindingKind::CatchParam,
            LangBindingKind::Assignment => BindingKind::Assignment,
            LangBindingKind::Loop => BindingKind::Loop,
            LangBindingKind::ContextManager => BindingKind::ContextManager,
            LangBindingKind::Comprehension => BindingKind::Comprehension,
            LangBindingKind::Type => BindingKind::Type,
            LangBindingKind::Receiver => BindingKind::Receiver,
            LangBindingKind::TypeParam => BindingKind::TypeParam,
            LangBindingKind::Module => BindingKind::Module,
            LangBindingKind::Trait => BindingKind::Trait,
        },
    }
}

/// The error a method the world declares but nothing has implemented yet returns.
///
/// Naming the method and what is missing is the whole value: a trap that says only
/// "unreachable" sends a reader to the guest, which is the one place the fault is not.
fn not_yet(method: &str, missing: &str) -> wasmtime::Error {
    wasmtime::Error::msg(format!(
        "`{method}` is declared by lanekeep:host@0.1.0 and not implemented yet: {missing} \
         lands in a later change. Trapping rather than answering, because every value this \
         could return instead is indistinguishable from a real answer."
    ))
}

impl HostCheckContext for HostState {
    fn file_path(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<String> {
        Ok(self.check_context_mut(&this)?.file_path.clone())
    }

    fn file_text(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<String> {
        Ok(self.check_context_mut(&this)?.arena.source().to_owned())
    }

    fn root(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<Handle> {
        // Resolved even though the answer does not depend on it, so this method cannot answer
        // for a context that is not there. Unreachable through the component model, which does
        // not deliver a method call on a dead borrow — but it is the one method where a
        // constant returned without a lookup would have been invisible, and every sibling
        // resolves first.
        self.check_context_mut(&this)?;

        // Zero, always, and `NodeArena` interns it first so that stays true. It is also the
        // reason `parent` returns `option<node>` rather than a sentinel.
        Ok(NodeArena::ROOT)
    }

    // --- navigation ---------------------------------------------------------------------
    //
    // Every one of these takes a handle and returns plain data. A handle that does not
    // resolve yields `none` or an empty list rather than trapping: rule code is arbitrary and
    // may pass any number, and a trap there would abort the run over a mistyped variable in a
    // rule.

    fn kind(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<String>> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .kind(n)
            .map(str::to_owned))
    }

    fn text(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<String>> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .text(n)
            .map(str::to_owned))
    }

    /// Unlike its JavaScript counterpart this cannot say "that handle resolves to nothing":
    /// the world declares `bool`, not `option<bool>`, so an unresolvable handle answers
    /// `false`. The two engines still agree wherever a rule can tell the difference —
    /// `undefined` is falsy, so `if (!ctx.isNamed(n))` takes the same branch under both — and
    /// the residue is a rule that compares against `undefined` explicitly, which would be
    /// asking a question about the handle rather than about the node.
    fn is_named(&mut self, this: Resource<CheckContext>, n: Handle) -> wasmtime::Result<bool> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .is_named(n)
            .unwrap_or(false))
    }

    fn line(&mut self, this: Resource<CheckContext>, n: Handle) -> wasmtime::Result<Option<u32>> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .position(n)
            .map(|(line, _)| line))
    }

    fn column(&mut self, this: Resource<CheckContext>, n: Handle) -> wasmtime::Result<Option<u32>> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .position(n)
            .map(|(_, column)| column))
    }

    fn parent(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<Handle>> {
        // `Some(0)` at every top-level node, and it must survive as `Some(0)`. The root's
        // handle is zero, so a host that folded absence and zero together would make every
        // top-level item in every file look parentless — silently, and only ever by removing
        // results.
        Ok(self.check_context_mut(&this)?.arena.parent(n))
    }

    fn children(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Vec<Handle>> {
        Ok(self.check_context_mut(&this)?.arena.children(n))
    }

    fn named_children(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Vec<Handle>> {
        Ok(self.check_context_mut(&this)?.arena.named_children(n))
    }

    fn ancestors(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Vec<Handle>> {
        Ok(self.check_context_mut(&this)?.arena.ancestors(n))
    }

    // --- positions and reporting ---------------------------------------------------------

    fn loc(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<NodeLocation>> {
        let context = self.check_context_mut(&this)?;
        // Nothing rather than a made-up position a reader would go and look at, which is the
        // same posture reporting at an unresolvable handle takes below.
        Ok(context
            .arena
            .position(n)
            .map(|(line, column)| NodeLocation {
                file: context.file_path.clone(),
                line,
                column,
            }))
    }

    /// Two independently optional parameters rather than the TypeScript API's union second
    /// argument, which existed only because JavaScript has no optional named parameters.
    ///
    /// `fix.safe` is `option<bool>` and unspecified means "a suggestion", read here rather
    /// than defaulted by whoever built the record — the same answer `read_fix` gives an
    /// absent `safe` property in JavaScript.
    fn report(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
        message: Option<String>,
        fix: Option<types::Fix>,
    ) -> wasmtime::Result<()> {
        let context = self.check_context_mut(&this)?;

        // A report at an unresolvable handle is dropped rather than recorded at a made-up
        // position. Reporting at 1:1 would point a reader at an unrelated line, which is
        // worse than the rule appearing not to have fired.
        let Some((line, column)) = context.arena.position(n) else {
            return Ok(());
        };

        // The range comes from the node the fix names rather than from offsets a rule
        // computed, which is the one mistake that would let a fix corrupt a file. A fix at a
        // handle that does not resolve is dropped and the report kept: what the rule found is
        // still true, and a replacement at guessed offsets would rewrite the wrong code.
        let fix = fix.and_then(|fix| {
            context.arena.byte_range(fix.node).map(|(start, end)| Fix {
                start,
                end,
                replacement: fix.text,
                // Absent means suggestion, decided here rather than by whatever authoring
                // crate built the record. The cautious mistake costs a manual edit; the other
                // one rewrites code silently. Identical to `lanekeep-js`'s `read_fix`, which
                // reads an absent `safe` property the same way.
                safe: fix.safe.unwrap_or(false),
            })
        });

        context.reports.push(Report {
            node: n,
            line,
            column,
            message,
            fix,
        });
        Ok(())
    }

    // --- binding resolution ----------------------------------------------------------------
    //
    // The light semantic layer of `docs/architecture.md` §6.4. A rule matching
    // `makeStyles(...)` on identifier text alone is wrong twice: it misses
    // `import { makeStyles as ms }`, and it fires on a local `const makeStyles` that has
    // nothing to do with the import.
    //
    // Nothing here traps, which is the difference from the group below. "That handle is not
    // live", "nothing declares that name" and "this language has no resolver" are all
    // *answers* a rule can act on, and the world's `bool` and `option<binding-kind>` carry
    // them. A trap is for a question the host cannot answer at all, and this is not one.

    fn resolves_to_import(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
        module: String,
        name: Option<String>,
    ) -> wasmtime::Result<bool> {
        Ok(self
            .check_context_mut(&this)?
            .binding(n)
            .is_some_and(|binding| binding.is_import_of(&module, name.as_deref())))
    }

    fn is_imported_from(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
        pattern: String,
    ) -> wasmtime::Result<bool> {
        Ok(self
            .check_context_mut(&this)?
            .binding(n)
            .is_some_and(|binding| binding.is_imported_from(&pattern)))
    }

    fn binding_kind(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<BindingKind>> {
        Ok(self
            .check_context_mut(&this)?
            .binding(n)
            .as_ref()
            .map(wit_kind))
    }

    fn is_shadowed(&mut self, this: Resource<CheckContext>, n: Handle) -> wasmtime::Result<bool> {
        let context = self.check_context_mut(&this)?;

        // Asked of the arena directly rather than through `binding`, because it is a different
        // question. `resolve_binding` answers "what does this name refer to"; this answers "is
        // there more than one and did you get the inner one" — which a name bound exactly once
        // answers `false` to while resolving perfectly well.
        Ok(context
            .resolver
            .as_deref()
            .is_some_and(|resolver| context.arena.is_shadowed(n, resolver)))
    }

    // --- declared, not yet implemented ----------------------------------------------------

    fn query_subtree(
        &mut self,
        _: Resource<CheckContext>,
        _: Handle,
        _: String,
    ) -> wasmtime::Result<Result<Vec<types::Match>, String>> {
        Err(not_yet("check-context.query-subtree", "the scoped queries"))
    }

    fn closest_ancestor(
        &mut self,
        _: Resource<CheckContext>,
        _: Handle,
        _: String,
    ) -> wasmtime::Result<Result<Option<types::Match>, String>> {
        Err(not_yet(
            "check-context.closest-ancestor",
            "the scoped queries",
        ))
    }

    fn read_file(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
    ) -> wasmtime::Result<Result<Option<String>, ReadError>> {
        Err(not_yet("check-context.read-file", "the tracked reads"))
    }

    fn file_exists(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
    ) -> wasmtime::Result<Result<bool, ReadError>> {
        Err(not_yet("check-context.file-exists", "the tracked reads"))
    }

    fn emit_fact(
        &mut self,
        _: Resource<CheckContext>,
        _: String,
        _: String,
    ) -> wasmtime::Result<Result<(), FactError>> {
        Err(not_yet("check-context.emit-fact", "the facts"))
    }

    fn today(&mut self, _: Resource<CheckContext>) -> wasmtime::Result<Option<String>> {
        Err(not_yet(
            "check-context.today",
            "the date a rule may observe",
        ))
    }

    fn drop(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

/// The cross-file phase, declared and not yet implemented.
///
/// It has to be here regardless: the world declares both resources, so the linker will not
/// accept a host that implements only one, and a rule with no reduce phase still exports a
/// `reduce` its language's scaffolding supplied.
impl HostReduceContext for HostState {
    fn files(&mut self, _: Resource<ReduceContext>) -> wasmtime::Result<Vec<String>> {
        Err(not_yet("reduce-context.files", "the cross-file phase"))
    }

    fn facts(
        &mut self,
        _: Resource<ReduceContext>,
        _: Option<String>,
    ) -> wasmtime::Result<Vec<EmittedFact>> {
        Err(not_yet("reduce-context.facts", "the cross-file phase"))
    }

    fn report(
        &mut self,
        _: Resource<ReduceContext>,
        _: ReduceLocation,
        _: Option<String>,
    ) -> wasmtime::Result<()> {
        Err(not_yet("reduce-context.report", "the cross-file phase"))
    }

    fn drop(&mut self, this: Resource<ReduceContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

/// The interface-level trait, which carries no methods of its own: everything
/// `lanekeep:host/types` declares hangs off one of the two resources.
impl Host for HostState {}
