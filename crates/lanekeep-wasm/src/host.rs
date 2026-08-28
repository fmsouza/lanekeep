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
//! # What is implemented
//!
//! All of it: `check-context`'s twenty-four methods and `reduce-context`'s three. Nothing on
//! this surface is a placeholder any more.
//!
//! **The placeholder's trap retires with it, and it leaves no gap behind.** Until now the
//! three reduce methods returned an error naming themselves, on the argument that every
//! plausible placeholder is a plausible *answer* — an empty list from `files` reads as "the run
//! considered nothing", an empty list from `facts` reads as "nothing was emitted". That was
//! right, and it is now moot. Nothing asserted it: `tests/world_shape.rs` is the only caller of
//! `call_reduce` and it drives a stub host, never this one, so the posture was never under
//! test. What replaces it is not a weaker assertion but `tests/reduce.rs`, which asserts what
//! each of the three now answers.
//!
//! **Two refusals below are decisions rather than placeholders, and they stay.** `read-file`
//! and `file-exists` fail the call on a host with no file access; `reduce-context.report` fails
//! it on a location with no line or column. Each has its own section, and each was decided on
//! its own evidence rather than by inheriting the argument above.
//!
//! **Binding resolution is not one of those, and the difference is not a relaxation.** `false`
//! and `none` are what "nothing resolves" *is*: a name nothing declares, a handle no arena
//! issued, and a language whose [`lanekeep_lang::Language::resolver`] returns `None` are three
//! ways of having no binding, and a rule acts on all three identically. The two refusals trap
//! because they have no answer to give; these have one.
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
//! decision this crate has to make rather than inherit. All three are made below, and they came
//! out **differently** — which is the point of making them one at a time.
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
//! # What `today` answers with no date: `none`, because the world already said so
//!
//! The third of the three, and the only one this file did not have to decide. `today` is
//! declared `option<string>` *with* the meaning of `none` written into it — "none when the rule
//! is not permitted to observe it" — so absence is a **declared answer** here rather than an
//! unrepresentable state. Neither move above applies: removing the state would contradict the
//! boundary, and trapping would refuse a question the world says has an answer. Reading the
//! world first is the pattern, and this is the case where reading it ends the argument.
//!
//! # The date is the one thing a rule may observe, so observing it is recorded
//!
//! What is left to decide is the half the cache depends on, and it is the sharper half.
//! `docs/architecture.md`'s determinism invariant is why the sandbox withholds `Math.random`,
//! `Date.now` and `new Date()` — a rule must not introduce nondeterminism even by accident —
//! and `today` is the single sanctioned exception. It is sanctioned only because the date is
//! *tracked*: [`CheckContext::date_was_read`] answers whether this file's result may be served
//! on another day, and `lanekeep-engine` already owns the mechanism that consumes it — its
//! `read_the_date` picks `RunKey::for_dated_file` over `RunKey::for_file`, which is what folds
//! the run's date into the key. Nothing here is a second pathway: this crate supplies the same
//! boolean, under the same name, that `lanekeep_js::HostContext::date_was_read` supplies.
//!
//! **Recorded whenever the method runs, including when the answer is `none`.** The two mistakes
//! are not symmetric. Over-recording dates one file's entry whose answer would not have changed,
//! costing a recompute; under-recording serves a stale answer indefinitely, which is the failure
//! the whole mechanism exists to prevent and the one nothing announces. A flag set at the top of
//! the only method that can set it is also auditable by reading it, where "set it unless the
//! answer was `none`" is a claim about a case production never reaches — `lanekeep-engine`
//! supplies a date for every file — and so a claim nothing would ever have falsified.
//!
//! There is no "it's only used for logging" exception, and there is no reading that does not
//! count. What a rule does with the answer is not this host's business; that it asked is.
//!
//! **The date is the caller's string, stored and returned unchanged.** `YYYY-MM-DD` is what
//! `wit/world.wit` documents and what `lanekeep_core::suppression::Date`'s `Display` renders,
//! and the caller fixes it once per run so two files checked a millisecond apart cannot disagree
//! about what day it is. This crate never reads a clock — there is none to read, since a
//! component built for `wasm32-unknown-unknown` imports no wall clock and this host has nothing
//! to offer one. Parsing or reformatting the string here and not in `lanekeep-js` would let one
//! engine refuse a run the other accepted, which is a disagreement about the run rather than
//! about the rule.
//!
//! # The reduce phase hands over two things, and the fact shape is not `lanekeep-js`'s
//!
//! [`ReduceContext`] holds a file list and a fact list, and there is nothing else on it to
//! hold: the reduce phase consumes facts and the file list and nothing else, which is what
//! keeps cross-file rules parallel and cacheable. The absence of a tree here is structural
//! rather than declined — `wit/world.wit` puts no navigation method on the resource, so a rule
//! holding one cannot ask.
//!
//! **Neither list is ordered here.** `files` returns the engine's list unchanged and `facts`
//! returns the stored order, filtered. The engine already sorts facts by
//! `(rule_id, file, sequence)` through [`lanekeep_core::fact::sort`] and already walks files in
//! a deterministic order, so a sort at this boundary would be a *second* ordering — and two
//! orderings that ever disagreed would make a rule that stops at the first match see a
//! different corpus depending on which one ran last. The check side takes the same posture:
//! [`CheckContext::take_facts`] is a plain `std::mem::take`, matching `lanekeep-js`.
//!
//! **The fact a rule reads carries its file in a field, not in its payload — and this is the
//! one place the two engines hand over genuinely different bytes.** `lanekeep-js` has nowhere
//! to put the file except inside the payload, because a JavaScript reduce phase receives parsed
//! objects and reads `f.file` off them; `lanekeep_js::merge_file` splices the key in, and
//! `lanekeep-engine` calls it at *reduce* time. The world has somewhere else to put it:
//! `emitted-fact` declares `file` as its own field. So a component rule reads `fact.file`, and
//! `fact.data` is the payload byte for byte as the guest emitted it.
//!
//! The protection that `merge_file` exists for is unchanged and the mechanism differs. Under
//! QuickJS the splice *overrides* a `"file"` the rule wrote itself, so a rule cannot attribute
//! its own fact to a file it did not come from. Here the authoritative field is the host's, set
//! from the context that emitted the fact, and a rule's own `"file"` key stays inert inside
//! `data` — unreachable as an attribution, because nothing reads `data` looking for one. What
//! must not happen is merging at emit time; [`HostState::emit_fact`] carries why, and it is a
//! literal duplicate key rather than an argument.
//!
//! **A context holds one rule's facts, and that selection is the engine's.** Letting one rule
//! read another's would turn a private payload shape into a contract between rules, and would
//! make the result depend on the order rules were declared in. `lanekeep-engine` already
//! filters by `rule_id` when it builds the QuickJS context, and nothing here needs to know it
//! happened.
//!
//! # What a reduce report answers with no position: the call fails
//!
//! The fourth decision of this shape and the second to land on refusal. It is also the first
//! where reading the world does not end the argument, so it is worth being explicit about why.
//!
//! `reduce-location` declares `line` and `column` as `option<u32>`, so unlike a grammar or a
//! `FileAccess` there is a representable "did not say" and something has to be decided about
//! it. The option is not a decision that a positionless report works — it mirrors the published
//! TypeScript `ReduceLocation`, whose `line?` and `column?` have been optional in the *type*
//! and required by the *runtime* since long before this world existed.
//!
//! Three things agree on the requirement, and none of them is this file. `docs/architecture.md`
//! §6.5 says the reduce form of `report` takes `{ file, line, column }`, and says why: there
//! are no nodes in that phase, so the position has to be captured during the per-file pass
//! while the tree is still there. `lanekeep_js::ReduceContext`'s `report` throws unless all
//! three are present, and tells the author exactly that. And [`lanekeep_core::Position`] has no
//! representation for an unknown line, so a report accepted without one has to acquire one
//! somewhere downstream.
//!
//! Downstream, the only thing to acquire is 1:1 — which is the one answer that actively
//! misleads. It points a reader at an unrelated line, and it is indistinguishable from a rule
//! that meant 1:1. A rule that genuinely means "the whole file" is entitled to say so; what it
//! may not do is leave the engine to say it on its behalf, invisibly. Refusing keeps that
//! decision with the rule, where it is visible in the rule's source.
//!
//! **Not a value on a `result` channel, and it should not become one.** `report` declares no
//! error case, so failing the call is the only refusal available — but even if the world were
//! open, a handleable refusal would let a rule branch on whether its report was accepted, and
//! there is nothing to branch to. Every alternative it could take is the 1:1 above with an
//! extra step.
//!
//! **Parity is the other half.** Under QuickJS the throw becomes `RunError::Rule` and the run
//! fails; here the trap does the same. Two engines that disagreed about whether a report was
//! accepted would make the same rule report on one and not on the other, which is a
//! disagreement about the run rather than about the rule.
//!
//! The residue is named rather than hidden: `file` itself is not checked, and an empty path is
//! accepted. `lanekeep-js` accepts one too — its `at.get::<_, String>("file")` succeeds for
//! `''` — and refusing it here would be this crate deciding a rule is wrong on evidence the
//! other engine does not have.
//!
//! # No interior mutability, and why that is not an oversight
//!
//! `lanekeep-js` holds its arena as `Rc<RefCell<NodeArena>>` because rquickjs requires
//! `'static` closures, so every host function must own a shared handle to the state it reads.
//! Nothing of the sort applies here: a host method receives `&mut self` and looks the context
//! up in the store's [`ResourceTable`], so it already holds the unique borrow the arena's
//! interning methods want. The plain `NodeArena` below is the same type used the simpler way,
//! not a second design.
//!
//! The date-read flag is the same story, and the one place copying the JavaScript spelling
//! across would have been worse than redundant. `lanekeep_js::HostContext` carries it as
//! `Rc<Cell<bool>>` for the reason above — an `Accessor` closure has to own a shared handle to
//! set it. Here it is set from an ordinary function body through `&mut self`, and an `Rc` would
//! not merely be unnecessary: `Rc` is not `Send`, so the `const` block above [`HostState`] would
//! stop this crate building. A plain `bool` is the same flag, held the way this side holds
//! everything else.

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

// No `CheckContext` or `ReduceContext` here, and their absence is the `with` mapping showing
// through: `bindings::types` re-exports each one as an alias for the type defined below, so
// importing either would collide with its own definition.
use crate::bindings::types::{
    self, BindingKind, EmittedFact, FactError, Host, HostCheckContext, HostReduceContext,
    NodeLocation, ReadError, ReduceLocation, StructureFingerprint,
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

/// A fact a rule emitted, before the engine attaches the file and rule it came from.
///
/// The fact analog of [`Report`] above, and deliberately not a [`lanekeep_core::Fact`] for the
/// same reason: a rule supplies a `kind` and a payload, and the identity around them — which
/// rule, which file, which position in that file's emission order — comes from the engine.
/// `lanekeep-engine` builds the full `Fact` from these three when it wires this crate, exactly
/// as it already does from `lanekeep_js::EmittedFact`.
///
/// Not the world's `emitted-fact` either, which is a different record for a different phase:
/// that one carries `file`, because it is what the *reduce* phase reads and the file is by then
/// known. Nothing merges a file into `data` here — see [`crate::host::HostState::emit_fact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The `kind` the reduce phase selects on. Never empty; a fact with an empty kind is
    /// refused rather than recorded.
    pub kind: String,
    /// The payload, exactly as the guest serialized it. Always a JSON object, checked rather
    /// than assumed, and never re-serialized.
    pub data: String,
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
    /// What the rule emitted for the reduce phase, in the order it emitted it.
    ///
    /// A `Vec` and never a set or a map: within a file the order a rule emitted in is the only
    /// order it can have meant, and [`lanekeep_core::fact::sort`] preserves it by assigning a
    /// sequence from this position. Deduplicating would be the engine inventing a claim the
    /// rule did not make.
    facts: Vec<Fact>,
    resolver: Option<Arc<dyn BindingResolver>>,
    /// Tracked, confined access to the rest of the project, when the host granted any.
    ///
    /// An `Option`, where [`CheckContext::language`] above is not, and that asymmetry is this
    /// module's second decision rather than an oversight. See the module header.
    ///
    /// **Shared, and it took a change in `lanekeep-core` to make that possible.** This was an
    /// owned `FileAccess`, because an `Arc<FileAccess>` was not `Send` while the memo was a
    /// `RefCell`, and the `const` block below requires this type to be `Send`. Owning it meant
    /// a second memo per file the moment both engines ran over one corpus — and the two lists
    /// cannot be merged afterwards, because `lanekeep_core::tracked::sort` orders by path and
    /// does not dedupe, so a disagreement about one path's hash becomes two contradictory
    /// entries for it.
    ///
    /// `FileAccess`'s memo is now a `Mutex`, so the engine builds one access per *file* and
    /// hands the same one to every rule checking that file, in either engine. Reading a file
    /// rewritten mid-run therefore has one answer per run rather than one per engine.
    files: Option<Arc<FileAccess>>,
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
    /// The date a rule sees through `today`, when the host supplied one.
    ///
    /// `None` is the world's own declared answer — "none when the rule is not permitted to
    /// observe it" — and not a stand-in for a missing one, which is why this is an `Option`
    /// where [`CheckContext::language`] is not. Stored exactly as the caller spelled it; see the
    /// module header on why nothing here parses it.
    today: Option<String>,
    /// Whether anything called `today` while checking this file.
    ///
    /// Tracked rather than assumed, because observing the date makes a file's result depend on
    /// what day it is. Assuming every file might read it would date every cache entry and
    /// invalidate the whole corpus daily; assuming none does would serve yesterday's answer.
    /// The same sentence `lanekeep_js::HostContext`'s field carries, because it is the same flag
    /// feeding the same key.
    ///
    /// A plain `bool` and not the JavaScript side's `Rc<Cell<bool>>` — see the module header's
    /// last section, where an `Rc` here does not compile rather than merely being redundant.
    date_read: bool,
}

impl std::fmt::Debug for CheckContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckContext")
            .field("file_path", &self.file_path)
            .field("language", &self.language.id())
            .field("interned_nodes", &self.arena.len())
            .field("reports", &self.reports.len())
            .field("facts", &self.facts.len())
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
            // Both, as `lanekeep_js::HostContext`'s `Debug` prints both, and they answer
            // different questions: whether this context can answer `today` at all, and whether
            // anything asked. The second is the cache-relevant one and is not derivable from
            // the first.
            .field("has_today", &self.today.is_some())
            .field("date_read", &self.date_read)
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
            facts: Vec::new(),
            resolver: None,
            files: None,
            language,
            queries: BTreeMap::new(),
            compiled: 0,
            today: None,
            date_read: false,
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
    /// Takes a shared access, and that is the answer to the question this doc comment used to
    /// leave open. **One per file shares the memo; one per rule does not, and the lists do not
    /// merge safely.** [`lanekeep_core::tracked::sort`] orders by path and *does not dedupe*, so
    /// two per-rule lists that disagree about one path's hash concatenate into two contradictory
    /// entries for it — and disagreeing is exactly what they do if the file was rewritten
    /// between the two rules, which is the case a shared memo exists to prevent.
    ///
    /// `lanekeep-engine` therefore builds one [`FileAccess`] per file and hands the same
    /// [`Arc`] to every rule on that file **in both engines**, which is what the signature is
    /// for: taking one by value would have made a second memo the only option the moment a
    /// TypeScript rule and a component rule met on one file.
    #[must_use]
    pub fn with_file_access(mut self, files: Arc<FileAccess>) -> Self {
        self.files = Some(files);
        self
    }

    /// Supply the date rules see through `today`.
    ///
    /// Fixed for the run by the caller and never read here: two files checked a millisecond
    /// apart must not disagree about what day it is, and this crate looks at no clock at all.
    /// Without one, `today` answers `none` — the world's own declared meaning for absence, and
    /// the reason this stays an `Option` where [`CheckContext::new`] makes the grammar
    /// mandatory.
    ///
    /// Takes the date as the caller spells it and stores it unchanged. `YYYY-MM-DD` is what
    /// `wit/world.wit` documents and what [`lanekeep_core::suppression::Date`] renders;
    /// validating it here and not in `lanekeep-js` would let one engine refuse a run the other
    /// accepted. The same posture — and the same signature —
    /// `lanekeep_js::HostContext::with_today` has.
    #[must_use]
    pub fn with_today(mut self, today: &str) -> Self {
        self.today = Some(today.to_owned());
        self
    }

    /// Whether anything read the date while checking this file.
    ///
    /// What a caller needs in order to decide whether this file's result may be served on
    /// another day: `lanekeep-engine` folds the run's date into the cache key for exactly the
    /// files that answer `true`, through `RunKey::for_dated_file` rather than `RunKey::for_file`.
    /// Named and shaped after `lanekeep_js::HostContext::date_was_read` because it is the same
    /// input to the same mechanism, and a second spelling is how a second pathway starts.
    ///
    /// `true` whenever `today` ran, including when it answered `none` — see the module header.
    /// Sticky for the life of the context: [`CheckContext::take_reports`] empties reports and
    /// [`CheckContext::take_facts`] empties facts, but nothing un-observes a read, so a context
    /// reused across several `check` calls carries the answer forward.
    #[must_use]
    pub const fn date_was_read(&self) -> bool {
        self.date_read
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
            .map(|files| files.dependencies())
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

    /// Take every fact emitted so far, in emission order, leaving the context empty.
    ///
    /// Emptying for the reason [`CheckContext::take_reports`] empties: a context read twice
    /// must not emit a fact twice. It matters more here than there, because a duplicated fact
    /// does not merely appear twice in output — it changes what a counting or first-seen-wins
    /// `reduce` concludes about the corpus.
    ///
    /// Emission order, and the engine turns that position into
    /// [`lanekeep_core::Fact::sequence`]. Assigned from the outside rather than carried here so
    /// that a rule cannot reorder its own facts relative to another file's.
    #[must_use]
    pub fn take_facts(&mut self) -> Vec<Fact> {
        std::mem::take(&mut self.facts)
    }
}

/// A cross-file violation a rule asked for.
///
/// The reduce-phase counterpart of [`Report`], carrying a `file` of its own instead of a node:
/// a cross-file rule reports at the site a fact came from, which is by definition not "the file
/// being checked" — there is not one. The same four fields as `lanekeep_js::ReduceReport`, and
/// a separate type for the reason [`Report`] is one.
///
/// `line` and `column` are plain `u32` where `wit/world.wit`'s `reduce-location` declares
/// `option<u32>`, and that narrowing is this module's decision rather than an oversight: a
/// report that named no site is refused at the boundary rather than recorded with one missing.
/// See the module header for the three sources that agree on the requirement and what accepting
/// it would have cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceReport {
    /// Path of the file to report against, as the rule gave it.
    ///
    /// Not checked against the corpus, and not checked for emptiness. A cross-file rule may
    /// legitimately report at a file the walker excluded — a config, a generated file — and
    /// `lanekeep-js` accepts the same paths this does.
    pub file: String,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// A message overriding the rule card's, when the rule supplied one.
    pub message: Option<String>,
}

/// Host state for the cross-file phase of one rule: the corpus as facts, and nothing else.
///
/// This is the representation `wit/world.wit`'s `reduce-context` resource is stored as — the
/// second type named in [`crate::bindings`]'s `with` mapping. It is the whole of what the
/// reduce phase can see, and the list of fields is the invariant: no arena, no source text, no
/// file access, because the reduce phase never touches parse trees.
///
/// Built per rule rather than per run, exactly as `lanekeep_js::ReduceContext` is. A rule sees
/// only its own facts; letting one read another's would make an internal payload shape into a
/// contract between rules, and would make the result depend on the order rules were declared
/// in. The filtering that achieves it belongs to the engine, which knows which rule emitted
/// what.
///
/// `Debug` is hand-written because the derived one would print every fact's payload — the whole
/// corpus, at whatever size the corpus is — for a line of diagnostic output. Counts are what a
/// reader of that line wants, on the same reasoning [`CheckContext`]'s hand-written `Debug`
/// prints `interned_nodes` rather than the arena.
pub struct ReduceContext {
    /// Every file the run considered, in the order the engine supplied.
    ///
    /// Returned unchanged: the engine's order is already deterministic, and sorting here would
    /// be a second ordering for the same list. See the module header.
    files: Vec<String>,
    /// This rule's facts, in the order the engine supplied.
    ///
    /// The world's own record, and not a mirror of it. `emitted-fact` *is* the reduce phase's
    /// fact — `kind`, the `file` it came from, and the payload as the guest serialized it —
    /// where [`Fact`] above is deliberately a different shape for a different phase, carrying
    /// no file because at emit time the engine has not attached one.
    facts: Vec<EmittedFact>,
    /// What the rule reported, in call order.
    reports: Vec<ReduceReport>,
}

impl std::fmt::Debug for ReduceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReduceContext")
            .field("files", &self.files.len())
            .field("facts", &self.facts.len())
            .field("reports", &self.reports.len())
            .finish()
    }
}

impl ReduceContext {
    /// Build a context over one rule's facts and the discovered file list.
    ///
    /// The same signature `lanekeep_js::ReduceContext::new` has, and the same responsibility on
    /// the caller: the facts are the ones this rule emitted, already ordered by
    /// [`lanekeep_core::fact::sort`], and the files are the run's own list.
    #[must_use]
    pub const fn new(files: Vec<String>, facts: Vec<EmittedFact>) -> Self {
        Self {
            files,
            facts,
            reports: Vec::new(),
        }
    }

    /// Take everything reported so far, leaving the context empty.
    ///
    /// Emptying rather than copying, for the reason [`CheckContext::take_reports`] empties: a
    /// context read twice must not report a violation twice.
    #[must_use]
    pub fn take_reports(&mut self) -> Vec<ReduceReport> {
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
    assert_send::<ReduceContext>();
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

    /// Whether the table currently holds no context at all.
    ///
    /// Evidence, in the sense [`crate::runtime::WasmRuntime::instantiations`] is: nothing in
    /// execution reads it, and it exists because "the engine gives each context back when it is
    /// finished with the file" is a claim only a number can carry. A store lives for a whole
    /// worker's share of the corpus and a context holds the parse tree and the file's entire
    /// source, so an engine that pushed one per file and never took it back would grow with the
    /// corpus, silently, until a run large enough to notice.
    #[must_use]
    pub fn holds_no_contexts(&self) -> bool {
        self.table.is_empty()
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

    /// Take a context back out of the table, freeing its entry.
    ///
    /// **The counterpart [`HostState::push_check_context`] needs in order to be called per
    /// file.** A store lives for a whole worker's share of the corpus, and a context holds a
    /// [`NodeArena`] — the parse tree and the file's whole source text. Pushing one per file and
    /// never taking it back is a table that grows with the corpus and memory that grows with it,
    /// charged against the same per-store ceiling `crate::runtime`'s `MemoryCeiling` enforces.
    /// Nothing traps and nothing is wrong until a large enough run; that is exactly the shape of
    /// leak that ships.
    ///
    /// It also ends the lend. A guest only ever receives a `borrow`, so no handle it holds can
    /// outlive the call — but the *host*'s own resource is what keeps the arena alive, and the
    /// engine has no other way to say it is finished with a file.
    ///
    /// # Errors
    ///
    /// Returns the table's error when the resource is not live, which for a call from the engine
    /// means it was already taken.
    pub fn take_check_context(
        &mut self,
        handle: Resource<CheckContext>,
    ) -> wasmtime::Result<CheckContext> {
        Ok(self.table.delete(handle)?)
    }

    /// Put a cross-file context in the table, so it can be lent to a guest.
    ///
    /// One table holds both kinds, and that is the component model's arrangement rather than a
    /// convenience: the guest's own handle space is per resource *type*, so a `check-context`
    /// and a `reduce-context` living in one [`ResourceTable`] are still two disjoint tables as
    /// far as a guest is concerned. A handle forged for the wrong one does not resolve — see
    /// `tests/reduce.rs`.
    ///
    /// # Errors
    ///
    /// Returns the table's error when it cannot accept another entry.
    pub fn push_reduce_context(
        &mut self,
        context: ReduceContext,
    ) -> wasmtime::Result<Resource<ReduceContext>> {
        Ok(self.table.push(context)?)
    }

    /// The cross-file context a resource names, for the caller that owns it.
    ///
    /// # Errors
    ///
    /// Returns the table's error when the resource is not live, on the terms
    /// [`HostState::check_context_mut`] sets out.
    pub fn reduce_context_mut(
        &mut self,
        handle: &Resource<ReduceContext>,
    ) -> wasmtime::Result<&mut ReduceContext> {
        Ok(self.table.get_mut(handle)?)
    }

    /// Take a cross-file context back out of the table, on the terms
    /// [`HostState::take_check_context`] sets out.
    ///
    /// One per rule rather than one per file, so the growth is bounded by the ruleset — but a
    /// reduce context holds every fact the corpus produced for that rule, which is the largest
    /// single value either phase carries.
    ///
    /// # Errors
    ///
    /// Returns the table's error when the resource is not live.
    pub fn take_reduce_context(
        &mut self,
        handle: Resource<ReduceContext>,
    ) -> wasmtime::Result<ReduceContext> {
        Ok(self.table.delete(handle)?)
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

/// The error a cross-file report with no line or column returns.
///
/// Names the file the rule did supply, because that is the one thing it said and it is what a
/// reader needs in order to find the `report` call that was wrong. See the module header for
/// why this is a refusal rather than a recorded report at 1:1.
fn reporting_without_a_position(file: &str) -> wasmtime::Error {
    wasmtime::Error::msg(format!(
        "`reduce-context.report` was called for `{file}` with no line or column. A cross-file \
         violation with no site is unactionable, and 1:1 is not a safe stand-in: it points a \
         reader at an unrelated line and is indistinguishable from a rule that meant 1:1. \
         Capture the position during the per-file pass, where the tree is still there, and \
         carry it on the fact. `lanekeep-js` refuses the same call the same way — its reduce \
         `ctx.report` throws unless `file`, `line` and `column` are all present, which ends \
         the invocation."
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

    fn structure_fingerprint(
        &mut self,
        this: Resource<CheckContext>,
        n: Handle,
    ) -> wasmtime::Result<Option<StructureFingerprint>> {
        Ok(self
            .check_context_mut(&this)?
            .arena
            .structure_fingerprint(n)
            .map(|fingerprint| StructureFingerprint {
                hash: fingerprint.hash,
                nodes: fingerprint.nodes,
            }))
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

    // --- facts -----------------------------------------------------------------------------
    //
    // The one method on this surface whose *contract* differs from its QuickJS counterpart
    // rather than only its calling convention. `JSON.stringify` made "the payload is a JSON
    // object" true by construction; a guest hands over a `string` and the host has to find out.
    // `crate::facts` owns that question and carries the reasoning.

    /// Record a fact for the reduce phase, or refuse it through the world's own error case.
    ///
    /// Nothing is recorded when the fact is refused, which is the half a permissive
    /// implementation gets wrong quietly: a malformed payload written into a cache entry
    /// surfaces on a later run, in the reduce phase, as far from the rule that emitted it as
    /// this design allows.
    ///
    /// **The file is not merged in here, and that is a departure from `lanekeep-js` worth
    /// stating.** Its `merge_file` splices a `"file"` key into a fact's serialized payload so a
    /// rule cannot misattribute a cross-file violation by shadowing it — but it is called by
    /// `lanekeep-engine` at *reduce* time, not at emit time, and the world removes the need for
    /// it: `emitted-fact` carries `file` as its own field, filled by the host from the context,
    /// where a rule's own `"file"` key is inert data inside `data`. Doing it here would put a
    /// `"file"` in the stored payload that the engine's reduce-time merge would then duplicate,
    /// and would make this engine's cached `data` differ from the other's for the same fact.
    fn emit_fact(
        &mut self,
        this: Resource<CheckContext>,
        kind: String,
        data: String,
    ) -> wasmtime::Result<Result<(), FactError>> {
        let context = self.check_context_mut(&this)?;

        // A value, not a trap. The world declares three cases and every one of them is a
        // property of what the *rule* sent — deterministic in `(bytes, path, ruleset, config,
        // tracked reads)` — so a rule handling one cannot make the run's output depend on
        // anything the cache key does not already cover. That is the whole difference from the
        // read methods above, where the missing thing was the host's own authority.
        if let Err(problem) = crate::facts::validate(&kind, &data) {
            return Ok(Err(problem));
        }

        // The guest's bytes, moved rather than re-rendered. A round trip through
        // `serde_json::Value` would substitute this workspace's map ordering for the guest's
        // own — see `crate::facts`.
        context.facts.push(Fact { kind, data });
        Ok(Ok(()))
    }

    // --- the date --------------------------------------------------------------------------
    //
    // The one thing on this whole surface that lets a rule observe anything outside
    // `(bytes, path, ruleset, config, tracked reads)`, and the only reason it is allowed is
    // that the observation is *recorded*. Everything else the determinism invariant withholds
    // stays withheld, structurally rather than by curation: a component built for
    // `wasm32-unknown-unknown` imports no wall clock, and this world declares none to import.

    /// The date the host supplied, and a note that this file's result now depends on it.
    ///
    /// The flag is set before the answer is read and whatever the answer turns out to be. A
    /// `none` that left no trace would be a rule reaching for the date surface with nothing
    /// recording that it did, which is the one failure this mechanism exists to prevent — see
    /// the module header for why the branch that would save a recompute is not worth having.
    fn today(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<Option<String>> {
        let context = self.check_context_mut(&this)?;
        context.date_read = true;

        // Verbatim. The caller's string is the run's answer, and re-rendering it here would put
        // this crate's idea of a date between the engine and the rule.
        Ok(context.today.clone())
    }

    fn drop(&mut self, this: Resource<CheckContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

/// The cross-file phase: three methods, and the shape of the set is the invariant.
///
/// There is no navigation here, no file text, no tracked read and no `emit-fact` — not
/// withheld, but absent from the resource `wit/world.wit` declares, so a rule holding one of
/// these cannot ask. That is what keeps cross-file rules parallel and cacheable: a `check` that
/// could read the corpus would make a file's result depend on files other than itself, and a
/// `reduce` that could emit facts could feed itself with no second pass for the result to
/// reach.
///
/// It has to be implemented regardless of whether a rule uses it: the world declares both
/// resources, so the linker will not accept a host that implements only one, and a rule with no
/// reduce phase still exports a `reduce` its language's scaffolding supplied.
impl HostReduceContext for HostState {
    /// The run's file list, unchanged.
    ///
    /// One crossing for the whole list, which is the shape this phase wants: `lanekeep-js`
    /// hands JavaScript one array built in one pass for the same reason, since a phase whose
    /// job is bulk work must not pay per item at the boundary.
    fn files(&mut self, this: Resource<ReduceContext>) -> wasmtime::Result<Vec<String>> {
        Ok(self.reduce_context_mut(&this)?.files.clone())
    }

    /// This rule's facts, optionally narrowed to one kind.
    ///
    /// `none` means every fact and an unknown kind means none of them — an empty list rather
    /// than anything a rule has to test for, so `for fact in ctx.facts("nope")` is a no-op
    /// instead of a failure. The same two answers `lanekeep-js` gives.
    ///
    /// Filtered, never sorted and never deduplicated. The order is the one the engine supplied,
    /// which it produced with [`lanekeep_core::fact::sort`]; a second ordering here could
    /// disagree with that one, and a rule that stops at the first match would then see a
    /// different corpus depending on which ran last.
    fn facts(
        &mut self,
        this: Resource<ReduceContext>,
        kind: Option<String>,
    ) -> wasmtime::Result<Vec<EmittedFact>> {
        let context = self.reduce_context_mut(&this)?;
        Ok(context
            .facts
            .iter()
            .filter(|fact| kind.as_ref().is_none_or(|wanted| *wanted == fact.kind))
            .cloned()
            .collect())
    }

    /// Record a cross-file violation, or fail the call when it names no site.
    ///
    /// The context is resolved first, as every sibling on both resources resolves first, so a
    /// dead handle is reported as a dead handle rather than as a bad location.
    fn report(
        &mut self,
        this: Resource<ReduceContext>,
        at: ReduceLocation,
        message: Option<String>,
    ) -> wasmtime::Result<()> {
        let context = self.reduce_context_mut(&this)?;

        // Both, and independently: a report carrying a line and no column is as unactionable as
        // one carrying neither, and `wit/world.wit` makes them two options rather than one
        // optional pair. Nothing is recorded — see the module header on why 1:1 is the one
        // stand-in that actively misleads.
        let (Some(line), Some(column)) = (at.line, at.column) else {
            return Err(reporting_without_a_position(&at.file));
        };

        context.reports.push(ReduceReport {
            file: at.file,
            line,
            column,
            message,
        });
        Ok(())
    }

    fn drop(&mut self, this: Resource<ReduceContext>) -> wasmtime::Result<()> {
        self.table.delete(this)?;
        Ok(())
    }
}

/// The interface-level trait, which carries no methods of its own: everything
/// `lanekeep:host/types` declares hangs off one of the two resources.
impl Host for HostState {}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;
    use lanekeep_lang_js::TypeScript;

    use super::*;

    fn context() -> CheckContext {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("the grammar loads");
        let tree = parser.parse("const x = 1;", None).expect("parses");
        CheckContext::new(
            NodeArena::new(tree, "const x = 1;".to_owned()),
            "src/a.ts",
            Arc::new(TypeScript),
        )
    }

    #[test]
    fn a_context_taken_back_leaves_the_table_empty() {
        // The engine pushes one of these per file into a store that lives for a whole worker's
        // share of the corpus, and each holds a parse tree and the file's entire source. Without
        // a way to give it back, a worker's memory grows with the corpus — silently, because
        // nothing traps and nothing is wrong until a run large enough to notice.
        let mut state = HostState::new();
        assert!(state.holds_no_contexts(), "a fresh state holds nothing");

        let handle = state.push_check_context(context()).expect("pushes");
        assert!(
            !state.holds_no_contexts(),
            "a pushed context is in the table"
        );

        let taken = state.take_check_context(handle).expect("takes");
        assert_eq!(taken.file_path(), "src/a.ts", "the context comes back");
        assert!(
            state.holds_no_contexts(),
            "taking a context back must free its entry"
        );
    }

    #[test]
    fn a_context_cannot_be_taken_twice() {
        // A `Resource` is an owned claim on a table entry, so this is only reachable by forging
        // a second one — which is exactly what a caller cloning a handle rather than moving it
        // would do, and it must fail rather than hand out a second copy of the same state.
        let mut state = HostState::new();
        let handle = state.push_check_context(context()).expect("pushes");
        let forged = Resource::new_own(handle.rep());

        drop(state.take_check_context(handle).expect("takes"));
        assert!(
            state.take_check_context(forged).is_err(),
            "an entry that is gone must not be handed out again"
        );
    }

    #[test]
    fn a_reduce_context_is_taken_back_the_same_way() {
        let mut state = HostState::new();
        let handle = state
            .push_reduce_context(ReduceContext::new(Vec::new(), Vec::new()))
            .expect("pushes");
        assert!(!state.holds_no_contexts());

        drop(state.take_reduce_context(handle).expect("takes"));
        assert!(
            state.holds_no_contexts(),
            "a cross-file context holds every fact the corpus produced for its rule"
        );
    }
}
