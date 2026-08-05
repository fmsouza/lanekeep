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
//! `check-context`'s navigation, reporting, binding resolution, scoped queries and tracked
//! reads. Everything else the world declares — facts, `today`, and the whole of
//! `reduce-context` — returns an error naming itself, which traps the call.
//!
//! **Trapping rather than answering, deliberately.** Every plausible placeholder is a
//! plausible *answer*: `none` from `read-file` reads as "the file is not there", an empty list
//! from `facts` reads as "nothing was emitted". A rule built against either would look like it
//! was working, and the run would be wrong quietly. An error is the one response no caller can
//! mistake for a result.
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
//! `readFile` and `fileExists` without file access, `today` without a date. Each of those is a
//! decision this crate has to make rather than inherit. The first two are made below, and they
//! came out **differently** — which is the point of making them one at a time; the third belongs
//! to the change that implements it.
//!
//! # What a query answers with no grammar: the question is removed
//!
//! `lanekeep-js` installs `querySubtree` and `closestAncestor` only when a language was
//! attached, so a rule that calls one without a grammar reaches a function that is genuinely
//! *absent* and gets a `TypeError` naming it. WIT has no absent export — every method the
//! world declares exists on every component — so that absence cannot cross the boundary, and
//! something has to be answered.
//!
//! It is answered by removing the state rather than by choosing a value for it.
//! [`CheckContext::new`] takes the grammar, so a context that cannot compile a query cannot be
//! built. This costs nothing and is not a coincidence: the file was parsed by *some* grammar
//! before any of this runs — "the grammar that parses a file is chosen by the file, not by the
//! rule" — and whoever parsed it is holding the [`Language`] that chose it. A caller who
//! forgets now gets a compile error instead of a run that is quietly wrong.
//!
//! Three alternatives were on the table. Each is worse, and in a different direction.
//!
//! - **An empty list, or `none`.** The cheapest, and the one this project has already been
//!   bitten by: an empty result set is a *plausible answer to a real question*, and a rule
//!   cannot tell it from "this file's language has no grammar for that query". A defect that
//!   only ever *removes* findings announces itself to nobody — `no-unwrap` lost its whole
//!   `#[test]` exemption to one, silently, and the rule went on passing its tests.
//! - **A trap.** Loud, which is the right instinct and the wrong tool here. It crashes a rule
//!   on a file it could have skipped, and it is not the rule's to catch: a wasm trap unwinds
//!   the instance, where the `result` the world already declares is a value the guest handles.
//! - **The world's own error case, carrying "this host has no grammar".** The closest of the
//!   three, and it fails on what the world *says*: `query-subtree`'s error is "the query
//!   compiler's message". A host misconfiguration rendered on that channel reaches a rule
//!   author as a query-authoring mistake in a query that is fine. Making it honest means
//!   editing `wit/world.wit`, whose bytes are a cache-key input — a real cost, paid to
//!   describe a state that did not have to exist.
//!
//! So the error case carries exactly what the world says it carries, and nothing else. That is
//! what `tests/queries.rs` asserts rather than merely asserting the error is non-empty.
//!
//! **What this does not do is make the two engines disagree.** `lanekeep-js` keeps its
//! `Option`, and its `None` branch is unreachable in production — `lanekeep-engine` builds
//! every `HostContext` with `.with_language(...)` — so there is no file a run can produce where
//! one engine has a grammar and the other does not.
//!
//! **The pattern, and its limit.** Read the world first, because it has often already decided.
//! `today` is declared `option<string>` *with* the meaning of `none` written into it — "none
//! when the rule is not permitted to observe it" — so absence there is a declared answer and
//! nothing needs removing. `read-error`'s three cases are all about the path and none of them
//! says "this host has no file access", so tracked reads faced the decision this one faced.
//! They did not reach the same answer, and an earlier draft of this paragraph said they had
//! only the same two ways out — remove the state, or change the world. There was a third, and
//! it is the one the next section takes.
//!
//! # What a read answers with no file access: the call fails
//!
//! The same shape of question, decided on its own evidence rather than inherited from the
//! section above.
//!
//! `lanekeep-js` installs `readFile` and `fileExists` only when a [`FileAccess`] was attached,
//! so a rule calling one without it reaches a function that is genuinely absent, gets a
//! `TypeError`, and `lanekeep-engine` turns that into `RunError::Rule` — the invocation ends and
//! the run fails. This host reproduces that refusal by the only means the boundary has: the two
//! methods return `Err`, which traps the call. Both engines refuse the same call, and both
//! refusals end the invocation. That parity is the requirement, because confinement is a
//! security property: two engines disagreeing about what is reachable is worse than either
//! answer alone.
//!
//! **Why not remove the state, as the grammar above does?** Because a grammar is *information*
//! about a file that has already been parsed, and file access is *authority*. "No grammar"
//! cannot exist — something parsed the file and is holding the [`Language`] that did. "No file
//! access" can, and under `docs/architecture.md` §13 it is a state the design may deliberately
//! want. Requiring a `FileAccess` would force every caller without one to fabricate one, and
//! there is no `FileAccess` that refuses everything: the nearest thing is one rooted at an empty
//! directory, which answers "that file is not there" to every question ever asked of it. That is
//! choosing a value to represent absence with an extra step, and the value it chooses is the
//! worst of the four. Nor is the caller hypothetical — `tests/world_shape.rs`,
//! `tests/bindings.rs`, `tests/navigation.rs` and `tests/queries.rs` each build a context with
//! no project root anywhere in sight, where every one of them was already holding the `Language`
//! the section above made mandatory.
//!
//! **Why not `none` and `false`?** They are the answers "the file is not there" and "it does not
//! exist". A rule cannot tell either from the truth, and one concluding "no tsconfig, so nothing
//! to check" would be silently right about a project it never looked at — the failure mode that
//! only ever *removes* findings, which announces itself to nobody.
//!
//! **Why not a fourth `read-error` case?** It is genuinely cheap — `read-error` is a `variant`,
//! so unlike the previous section's documented `string` channel a case can be added without
//! overloading anything — and it is still wrong, for a reason that is not about cost. It would
//! make host configuration **handleable**: a rule could catch `no-file-access` and report
//! something. The run's output would then depend on whether the host granted file access, which
//! is not one of `(bytes, path, ruleset, config, tracked reads)` and so is not in the cache key
//! — two hosts, two answers, one cache entry, and nothing that would ever invalidate it. A
//! failed call cannot be handled, so no output can depend on it. The world edit would also cost
//! what world edits cost: `wit/world.wit`'s bytes are a cache-key input, and four authoring
//! sub-projects bind against its shape.
//!
//! The residue is real and is named rather than hidden. A rule that reads a file on a host with
//! no file access takes the run down, where a rule told "you may not look" could have skipped
//! the file and carried on. That is exactly what happens today under QuickJS, and it is not the
//! state any wired engine produces: `lanekeep-engine` builds a `FileAccess` per file
//! unconditionally, so the branch is unreachable in production for the same reason
//! `with_language`'s is.
//!
//! # No interior mutability, and why that is not an oversight
//!
//! `lanekeep-js` holds its arena as `Rc<RefCell<NodeArena>>` because rquickjs requires
//! `'static` closures, so every host function must own a shared handle to the state it reads.
//! Nothing of the sort applies here: a host method receives `&mut self` and looks the context
//! up in the store's [`ResourceTable`], so it already holds the unique borrow the arena's
//! interning methods want. The plain `NodeArena` below is the same type used the simpler way,
//! not a second design.

use std::collections::BTreeMap;
use std::sync::Arc;

use lanekeep_core::files::{FileAccess, ReadError as FileReadError};
use lanekeep_core::fix::Fix;
use lanekeep_core::tracked::TrackedRead;
use lanekeep_lang::Language;
use lanekeep_lang::binding::{Binding, BindingKind as LangBindingKind, BindingResolver};
use lanekeep_nodes::{Handle, NodeArena};
use lanekeep_query::CompiledQuery;
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
    /// Tracked, confined access to the rest of the project, when the host granted any.
    ///
    /// An `Option`, where [`CheckContext::language`] above is not, and that asymmetry is this
    /// module's second decision rather than an oversight. See the module header.
    ///
    /// Owned rather than shared. `lanekeep-js` holds an `Rc<FileAccess>` because the engine
    /// builds one per *file* and hands it to every rule checking that file; an `Rc` here would
    /// take `Send` away from a type the `const` block below requires to have it, and an
    /// `Arc<FileAccess>` is not `Send` either, since `FileAccess`'s memo is a `RefCell`. Owning
    /// it keeps the reads structurally attached to the context that made them — which is what
    /// `files.rs` asks for — and leaves the engine free to decide, when it is wired, whether a
    /// context is built per file or per rule.
    files: Option<FileAccess>,
    /// The grammar the two query methods compile against.
    ///
    /// Not an `Option`, and that is this module's answer to the question `lanekeep-js` answers
    /// by not installing the functions at all. See the module header.
    language: Arc<dyn Language>,
    /// Queries compiled so far this file, by source.
    ///
    /// A rule that calls `query-subtree` inside a handler calls it once per match, with the
    /// same query string every time. Compiling per call would make the second-cheapest
    /// operation in the host the most expensive one. Failures are cached too, so a bad query
    /// is reported once rather than recompiled on every match.
    ///
    /// `BTreeMap` rather than `HashMap` for the reason `lanekeep-js` uses one: nothing here
    /// iterates it today, and a map whose order is a function of a hash seed is the kind of
    /// thing that starts being iterated later.
    queries: BTreeMap<String, Result<Arc<CompiledQuery>, String>>,
    /// How many compilations that cache has actually performed.
    compiled: usize,
}

impl std::fmt::Debug for CheckContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckContext")
            .field("file_path", &self.file_path)
            .field("language", &self.language.id())
            .field("interned_nodes", &self.arena.len())
            .field("reports", &self.reports.len())
            .field("has_resolver", &self.resolver.is_some())
            // Whether reads are possible, and not how many were made. `dependencies()` builds a
            // `Vec`, clones a `FilePath` per entry and sorts it, which is a lot of work to print
            // one number — and it takes a `RefCell` borrow inside a `Debug` impl, which is the
            // kind of thing that is safe until the day something formats while holding one.
            // `lanekeep-js`'s `HostContext` prints exactly this field and no count either.
            .field("has_file_access", &self.files.is_some())
            // Both, because they answer different questions: how many distinct sources this
            // file has seen, and how many of those actually reached the query compiler. Equal
            // is the healthy state; a `queries_compiled` that outruns `queries_cached` is the
            // cache not being read.
            .field("queries_cached", &self.queries.len())
            .field("queries_compiled", &self.compiled)
            .finish()
    }
}

impl CheckContext {
    /// Build a context over a parsed file, against the grammar that parsed it.
    ///
    /// Takes an arena rather than a `tree_sitter::Tree` and a source, which is one of two
    /// places this differs from `lanekeep_js::HostContext::new`. Both build the identical
    /// [`NodeArena`]; taking it already built keeps `tree-sitter` out of this crate's public
    /// API, and the caller that parses the file is holding one anyway.
    ///
    /// The other difference is `language`, which is required here and optional there. That is
    /// the module header's decision and not an inconvenience of the signature: WIT cannot make
    /// `query-subtree` absent for a context that has no grammar, so this crate does not let one
    /// exist. The caller that parsed the file is holding this too.
    #[must_use]
    pub fn new(arena: NodeArena, file_path: &str, language: Arc<dyn Language>) -> Self {
        Self {
            arena,
            file_path: file_path.to_owned(),
            reports: Vec::new(),
            resolver: None,
            files: None,
            language,
            queries: BTreeMap::new(),
            compiled: 0,
        }
    }

    /// How many distinct query sources this context has compiled.
    ///
    /// Here so the compile-once-per-source property is *checkable* rather than assumed. The
    /// size of the cache would not do it: an implementation that recompiled on every call and
    /// overwrote the entry ends up with the same map and a different amount of work, and the
    /// difference between those two is the whole property.
    #[must_use]
    pub const fn queries_compiled(&self) -> usize {
        self.compiled
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

    /// Allow tracked, confined reads of the rest of the project.
    ///
    /// Without one, `read-file` and `file-exists` fail the call rather than answering — the
    /// same refusal `lanekeep-js` expresses by not installing the functions at all. The module
    /// header carries why that is the answer here and what the alternatives cost.
    ///
    /// Takes the access by value, and the reads it records are this context's. Whether the
    /// engine builds one context per file or one per rule is its decision when it is wired, and
    /// nothing here presumes either — but the two are not equally cheap, and the difference is
    /// worth knowing before it is chosen.
    ///
    /// **One per file shares the memo; one per rule does not, and the lists do not merge
    /// safely.** [`lanekeep_core::tracked::sort`] orders by path and *does not dedupe*, so two
    /// per-rule lists that disagree about one path's hash concatenate into two contradictory
    /// entries for it — and disagreeing is exactly what they do if the file was rewritten
    /// between the two rules, which is the case a shared memo exists to prevent. Merging is
    /// sound only when the lists already agree. One context per file avoids the question
    /// entirely, and shares the arena and the query cache while it is there.
    #[must_use]
    pub fn with_file_access(mut self, files: FileAccess) -> Self {
        self.files = Some(files);
        self
    }

    /// Every file this context reached for, in path order.
    ///
    /// What a cache entry for this file depends on besides the file itself. Three things it
    /// records that are easy to leave out, all of them inherited from [`FileAccess`] rather
    /// than decided here: a read that found nothing is recorded with no hash, because a rule
    /// told "not there" has depended on that answer and must be re-run when the file appears; a
    /// file read twice under two spellings is one entry; and a *refused* read is not an entry
    /// at all, having produced no answer to depend on.
    ///
    /// Empty for a context with no file access, which is the truth about what was read rather
    /// than a stand-in for the missing authority. The rule-facing answer to that state is not
    /// an empty value — it is a failed call.
    ///
    /// **The "a refusal is not a dependency" claim has one known hole, and it is `FileAccess`'s
    /// rather than this crate's.** Two of the three refusals are properties of the path string,
    /// so nothing a later run does can change them. The third is not: `EscapesRoot` also fires
    /// *after* canonicalizing, when an in-root path resolves outside the root through a symlink
    /// — filesystem state, which a later run can change by replacing the link with an ordinary
    /// file. Nothing is recorded, so nothing invalidates the entry. Filed against
    /// `lanekeep-core`, where it affects both engines and is fixed once; it is more *reachable*
    /// here only because a refusal crosses this boundary as a value a rule can act on, where
    /// under QuickJS it throws and ends the invocation.
    #[must_use]
    pub fn dependencies(&self) -> Vec<TrackedRead> {
        self.files
            .as_ref()
            .map(FileAccess::dependencies)
            .unwrap_or_default()
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

    /// Compile a query, or hand back the outcome this file already saw for it.
    ///
    /// The cache is checked first, **including a cached failure**, which is the shape
    /// `lanekeep-js`'s `compile` has and the reason it has it: a rule calling `query-subtree`
    /// from inside its handler calls it once per match with the same source every time, and a
    /// query that does not compile does not compile any harder the fourth time.
    ///
    /// `Arc` rather than `Rc`, which is the one difference from the JavaScript engine's copy.
    /// QuickJS is single-threaded and its host functions are `'static` closures; nothing here
    /// is either, and rules run across rayon's pool, so an `Rc` in the store's data would take
    /// `Send` away from a type that has it. Enforced rather than asserted: the `const` block
    /// above [`HostState`] holds that claim, without which an `Rc` here compiles clean and
    /// stays clean until the engine is wired to rayon, at which point the reason is
    /// rediscovered as a confusing error somewhere else.
    fn compile(&mut self, source: &str) -> Result<Arc<CompiledQuery>, String> {
        if let Some(found) = self.queries.get(source) {
            return found.clone();
        }

        self.compiled += 1;
        let compiled = CompiledQuery::compile(self.language.as_ref(), source)
            .map(Arc::new)
            .map_err(|error| error.to_string());
        self.queries.insert(source.to_owned(), compiled.clone());
        compiled
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

/// The store's data stays `Send`, checked at compile time rather than believed.
///
/// A [`wasmtime::Store`] is `Send` when its data is, and the engine will run rules across
/// rayon's pool — so a `Store<HostState>` that has to move to a worker needs this. Nothing in
/// the type system asks for it *yet*, which is exactly the problem: the crate builds today
/// whether or not it holds, so dropping an `Rc` into [`CheckContext`] — the query cache is the
/// obvious candidate, since `lanekeep-js`'s equivalent is an `Rc` — compiles clean and stays
/// clean right up until the engine is wired, where it surfaces as an unsatisfied bound on a
/// rayon call far from the field that caused it.
///
/// A `const` block rather than a test: a test that does not compile is not a test that fails,
/// it is a build that stops, and either way this is a property of the types rather than of any
/// behavior. It costs nothing at runtime and names the invariant at the place it protects.
///
/// Anonymous — `const _` rather than a name — because a named constant nothing reads is dead
/// code, and silencing that lint would be a second claim to maintain.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<HostState>();
    assert_send::<CheckContext>();
};

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

/// Turn one match's capture paths into handles, once the tree borrow has ended.
///
/// A `list<match-entry>` and not a record, because a query's capture names are the rule's own
/// and are not known to the world in advance. Order is tree-sitter's reporting order, which is
/// deterministic for a given tree and query; nothing may depend on it beyond that, and a guest
/// reads a capture by name.
///
/// A capture whose path no longer resolves is dropped rather than handed over as a sentinel:
/// there is no `node` value meaning "not a node" — handle zero is the root — so the entry
/// simply is not there, which is what the world's own note on `match` describes.
fn intern(arena: &mut NodeArena, captures: Vec<(String, Vec<u32>)>) -> types::Match {
    captures
        .into_iter()
        .filter_map(|(name, path)| {
            arena
                .intern_path(path)
                .map(|node| types::MatchEntry { name, node })
        })
        .collect()
}

/// How a refusal is spelled at the boundary.
///
/// The world's `read-error` is a `variant` where [`lanekeep_core::files::ReadError`] is an
/// `enum` with named fields, so this is a change of shape and not of meaning: the same three
/// cases, each still carrying the path as the rule wrote it, which is what makes the message a
/// rule renders match the one QuickJS produces.
///
/// The match is exhaustive, so a fourth case added to `FileAccess` stops this crate from
/// building rather than quietly becoming one of the three — which matters more here than
/// almost anywhere else in this file, because a refusal silently re-labeled as a different
/// refusal is a confinement failure that still looks like a refusal.
fn wit_read_error(error: FileReadError) -> ReadError {
    match error {
        FileReadError::EscapesRoot { path } => ReadError::EscapesRoot(path),
        FileReadError::Absolute { path } => ReadError::Absolute(path),
        FileReadError::NotText { path } => ReadError::NotText(path),
    }
}

/// The error a read method returns when the host granted no file access at all.
///
/// Fails the call rather than answering, for the reason the module header sets out: every value
/// this could return instead is a plausible answer to the question that was asked.
fn no_file_access(method: &str) -> wasmtime::Error {
    wasmtime::Error::msg(format!(
        "`{method}` was called on a check-context built without file access, so this host \
         cannot answer it. Not `none` and not `false`: both are ordinary answers meaning the \
         file is not there, and a rule cannot tell either from the truth. `lanekeep-js` refuses \
         the same call the same way — without file access the function is absent, and calling \
         it is a TypeError that ends the invocation."
    ))
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

    // --- scoped queries --------------------------------------------------------------------
    //
    // Neither of these traps, on any input. A query that does not compile answers through the
    // world's own error case; a handle that resolves to nothing answers empty, as every
    // navigation method above does; an upward walk that finds nothing answers `none`. There is
    // no fourth case, because there is no context without a grammar to produce one.
    //
    // Both compile through one cache, keyed by source. `closest-ancestor` compiles before it
    // walks, even from a node with no ancestors at all, so a rule that wrote a bad query is
    // told so whatever node it asked from — an error that depended on the node's depth would
    // be a query bug that only some files reported.

    fn query_subtree(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
        query: String,
    ) -> wasmtime::Result<Result<Vec<types::Match>, String>> {
        let context = self.check_context_mut(&this)?;
        let compiled = match context.compile(&query) {
            Ok(compiled) => compiled,
            Err(problem) => return Ok(Err(problem)),
        };

        // Two phases, which the arena's ownership of the tree forces: capture paths are
        // collected while the tree is borrowed, then interned once that borrow has ended. A
        // handle cannot be minted while a `Node` derived from the same tree is still alive.
        //
        // An empty `match` in this list is a real value and not a way of saying "no match":
        // every capture of a pattern can fail to intern, and a rule reading an empty capture
        // list as "nothing matched" would be reading the wrong thing. The list's *length* is
        // what says how many matched. This is the distinction `closest-ancestor` cannot express
        // in a list at all, which is why its return is an `option`.
        let matches = context.arena.query_subtree(n, &compiled);
        Ok(Ok(matches
            .into_iter()
            .map(|captures| intern(&mut context.arena, captures))
            .collect()))
    }

    fn closest_ancestor(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
        query: String,
    ) -> wasmtime::Result<Result<Option<types::Match>, String>> {
        let context = self.check_context_mut(&this)?;
        let compiled = match context.compile(&query) {
            Ok(compiled) => compiled,
            Err(problem) => return Ok(Err(problem)),
        };

        let Some(captures) = context.arena.closest_ancestor_paths(n, &compiled) else {
            // `none`, on the world's own stated reason: an empty object is truthy in
            // JavaScript, so a "nothing matched" value shaped like a match takes the wrong
            // branch under `if (!...)`. Every authoring language reads the same `option`.
            //
            // Not because an empty `match` could arrive here and be confused with this —
            // `closest_ancestor_paths` only ever returns a match that captured the ancestor,
            // so `Some(vec![])` is unreachable through this method. The defensive shape is
            // still right; the argument for it belongs to `query-subtree` above, where an
            // empty capture list genuinely does occur.
            return Ok(Ok(None));
        };

        Ok(Ok(Some(intern(&mut context.arena, captures))))
    }

    // --- tracked reads ---------------------------------------------------------------------
    //
    // Both delegate straight into `FileAccess`, which owns confinement and tracking for every
    // engine rather than for one of them. Nothing about the rules is restated here: a path is
    // refused, read, or found absent by the same code `lanekeep-js` calls, so the question "can
    // a component rule reach a file a TypeScript rule cannot" has no place to be answered
    // differently.
    //
    // The two ways out are not interchangeable. A *refusal* is a value the world declares and
    // the rule handles; the *absence of file access* fails the call, because it is not about
    // the path and there is no answer to give.

    fn read_file(
        &mut self,
        this: Resource<CheckContext>,
        path: String,
    ) -> wasmtime::Result<Result<Option<String>, ReadError>> {
        let context = self.check_context_mut(&this)?;
        let Some(files) = context.files.as_ref() else {
            return Err(no_file_access("check-context.read-file"));
        };

        // `Ok(None)` is a read that found nothing, and it stays distinguishable from a refusal
        // — a rule asking whether a config is present should not have to handle an error to
        // find out. That is why the world's return is `result<option<string>, ...>` rather than
        // an error case carrying "not found".
        Ok(files.read(&path).map_err(wit_read_error))
    }

    fn file_exists(
        &mut self,
        this: Resource<CheckContext>,
        path: String,
    ) -> wasmtime::Result<Result<bool, ReadError>> {
        let context = self.check_context_mut(&this)?;
        let Some(files) = context.files.as_ref() else {
            return Err(no_file_access("check-context.file-exists"));
        };

        // Not `read(...).is_some()`. A file that exists and is not text still exists, and
        // `FileAccess::exists` answers the question that was asked — where `false` would claim
        // something untrue about the filesystem.
        Ok(files.exists(&path).map_err(wit_read_error))
    }

    // --- declared, not yet implemented ----------------------------------------------------

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
