//! The seam every type answer crosses.
//!
//! [`TypeScriptOracle`](crate::TypeScriptOracle) borrows one tree for its whole life, which
//! is the right shape for a within-file oracle and the wrong one for anything
//! whole-program: a provider backed by a compiler holds state the *run* owns and cannot hand
//! back a value borrowing a single parse. So the boundary is a trait whose methods borrow
//! nothing beyond the call, and everything a question needs travels in one [`Query`].
//!
//! Every arm may answer `None`, and `None` is this crate's first-class "I could not be sure"
//! rather than a failure — see the crate documentation. A provider that guessed would
//! produce a rule that accuses correct code, which is the one failure the whole surface is
//! arranged against.

use lanekeep_core::FilePath;
use lanekeep_core::files::FileAccess;

use crate::types::{Symbol, Type};

/// One question, with everything answering it needs.
///
/// `Copy`, so an arm can hand the same question to a helper without a clone and without
/// borrowing itself into a corner.
#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    /// The file the question is about, relative to the project root.
    ///
    /// A relative specifier resolves against this, so it is load-bearing rather than
    /// informational: the same import written in two directories names two files.
    pub file: &'a FilePath,
    /// That file's parse.
    pub tree: &'a tree_sitter::Tree,
    /// That file's source, which every byte range in `tree` indexes.
    pub source: &'a str,
    /// The node asked about. The file's root for [`TypeProvider::complete`], which asks
    /// about the whole file rather than about a position in it.
    pub node: tree_sitter::Node<'a>,
    /// Tracked, confined access to the rest of the project.
    ///
    /// Every file a provider opens goes through this, hit or miss, so a declaration an
    /// answer depended on — including one that was **not** there — invalidates the asking
    /// file's cache entry when it appears, changes or vanishes. A provider reading the
    /// filesystem any other way would compute an answer no cache key covers.
    pub files: &'a FileAccess,
}

/// What answers `ctx.types`.
///
/// One implementation per strategy — the bounded builtin oracle here, a `tsc` sidecar in
/// A3 — and the engine holds exactly one for a run. [`Self::identity`] is why the two cannot
/// be confused by a cache: it is a key input, so a result computed by one is never served to
/// a run using the other.
pub trait TypeProvider: Send + Sync {
    /// The type of the expression at `q.node`.
    fn type_of(&self, q: Query<'_>) -> Option<Type>;

    /// Where the name at `q.node` came from.
    fn symbol_of(&self, q: Query<'_>) -> Option<Symbol>;

    /// What calling the function at `q.node` yields.
    ///
    /// Separate from [`Self::type_of`] because a function declaration is not an expression,
    /// and giving `type_of` a signature type would invent a variant every rule would then
    /// have to unpack. Accepts a call expression, a function-like declaration, or an
    /// identifier bound to one.
    fn return_type_of(&self, q: Query<'_>) -> Option<Type>;

    /// Whether the type at `q.node` is the type `module` exports as `name`, or declares a
    /// relationship to it — `extends` or `implements` — across files, through aliases of the
    /// named type.
    ///
    /// Nominal, never structural. `Some(false)` is a real answer — the walk completed and
    /// found nothing — and `None` is "a link in the chain could not be read", which a rule
    /// must not treat as a negative. A union is assignable only when every member is.
    ///
    /// Three narrowings of what "found nothing" honestly covers: declaration merging is not
    /// followed, so when a name is declared more than once at a file's top level only the
    /// first declaration is consulted; a generic annotation at the use site (`let x:
    /// Box<number>`) answers `None`, since type arguments are not read; and a `name` the
    /// named `module` does not export answers `None` rather than `Some(false)`, because
    /// `Some(false)` there would make a requirement rule report on every value the module
    /// never claimed to type.
    ///
    /// One documented gap: declarations are looked up at a file's top level only, so a
    /// declaration that shadows the target's name inside a function body is not
    /// distinguished from the top-level one with that name — the walk answers as if the
    /// shadow were the top-level declaration.
    fn is_assignable_to(&self, q: Query<'_>, module: &str, name: &str) -> Option<bool>;

    /// Whether every import in `q`'s file resolved to something this provider could read.
    ///
    /// `false` is the honest label on a partial answer: a rule that reports on an absent
    /// type would accuse code the provider never saw. Takes a whole [`Query`] rather than a
    /// path because the question is about the file's *imports*, which cannot be enumerated
    /// without its tree.
    ///
    /// **A whole-file verdict, deliberately coarse.** One unreadable import makes the whole
    /// file `false`, and an import whose declaration file carries a single `ERROR` node
    /// anywhere counts as unreadable — so one unparsed construct in a fifty-thousand-line
    /// `@types` bundle makes every file that imports it incomplete, project-wide. Silence is
    /// the safe direction, and a narrower verdict (an `ERROR` covering the *asked* name) is a
    /// refinement filed with the resolver's own issue rather than a promise made here.
    fn complete(&self, q: Query<'_>) -> bool;

    /// What this provider *is*, for the cache key.
    ///
    /// Folded into `analysis_hash` by the engine. Two providers that would answer
    /// differently must return different bytes, and a provider whose own code changed must
    /// too — the builtin one derives it from `oracle_identity`, which digests this crate's
    /// `src/`, for exactly that reason.
    fn identity(&self) -> Vec<u8>;

    /// Prepare for a run over the corpus `files` yields, and answer the per-run key term.
    ///
    /// Called by the engine once per run, after discovery and before the run key is
    /// computed, for a fresh provider and a held one alike (plan 6). The default is nothing
    /// to build and no term: this provider's dependencies are tracked reads on each entry.
    /// The `tsc` provider (plan 5) overrides it to build every program the run's files
    /// belong to and answer the hash over their file lists and contents — that provider's
    /// whole dependency mechanism (spec §5.6) — which is why the term is asked for through
    /// the trait rather than read off a concrete type the engine would have to downcast to.
    /// Plan 5 adds the run's `AnalysisBudget` as a second parameter when that type exists.
    ///
    /// **The list is a closure, and the default body never calls it.** Producing it costs the
    /// engine a second walk of the whole project, on the warm path, for a provider that may
    /// have no use for it — which is what the only provider that ships today does. A `&[…]`
    /// parameter makes that walk unconditional; a `&dyn Fn` makes it the caller's cost only
    /// when a provider asks.
    ///
    /// What the closure yields is the corpus **as discovered**, not the run's `--since` or
    /// `--staged` selection: a program is a property of the project, and building one from a
    /// changed-files subset would answer a different question on a warm run than on a cold
    /// one.
    ///
    /// # Errors
    ///
    /// [`BeginRunError::Timeout`] when the work outlived its budget and
    /// [`BeginRunError::Failed`] for anything else. Both cancel the run, and both are
    /// reported as `RunError::Provider` until plan 5's `AnalysisTimeout` exists to tell them
    /// apart — so the distinction is for the provider's own message today, not for the
    /// engine's exit path.
    fn begin_run(&self, files: &dyn Fn() -> Vec<FilePath>) -> Result<Vec<u8>, BeginRunError> {
        let _ = files;
        Ok(Vec::new())
    }
}

/// Why [`TypeProvider::begin_run`] could not prepare a run.
///
/// Both variants are reported as `RunError::Provider` today; plan 5's `AnalysisTimeout`
/// is what will separate them at the engine's boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginRunError {
    /// The provider's work outlived the analysis budget.
    Timeout(String),
    /// The provider could not do its work at all.
    Failed(String),
}
