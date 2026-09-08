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

use lanekeep_core::files::FileAccess;
use lanekeep_core::{AnalysisBudget, FilePath};

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

    /// Drop whatever this provider holds that the files no longer support.
    ///
    /// Called once per request by a session that holds this provider across several of them
    /// (`crates/lanekeep-cli/src/session.rs`). A one-shot run builds a provider and throws it
    /// away, so for that path this is a no-op that costs a virtual call.
    ///
    /// The default body is empty because a provider with no cache has nothing to revalidate,
    /// and requiring every implementor to write that down would be noise. The two that do
    /// hold something override it: the builtin provider re-hashes each declaration it parsed
    /// and forgets its memoized completeness, and the `tsc` provider does nothing here because
    /// `begin_run` already re-answers `programs` on every prepare, held provider or not.
    /// **Nothing here reads an mtime** — held state is keyed by content hash, which is the
    /// only reason it is allowed to be held at all (architecture §8.2).
    fn revalidate(&self, files: &FileAccess) {
        let _ = files;
    }

    /// Prepare for a run over the corpus `files` yields, and answer the per-run key term.
    ///
    /// Called by the engine once per run, after discovery and before the run key is
    /// computed, for a fresh provider and a held one alike (plan 6). The default is nothing
    /// to build and no term: this provider's dependencies are tracked reads on each entry.
    /// The `tsc` provider (plan 5) overrides it to build every program the run's files
    /// belong to and answer the hash over their file lists and contents — that provider's
    /// whole dependency mechanism (spec §5.6) — which is why the term is asked for through
    /// the trait rather than read off a concrete type the engine would have to downcast to.
    /// The run's [`AnalysisBudget`] is the second parameter: a provider that spends wall
    /// clock time preparing must spend the *run's* budget rather than one of its own, so a
    /// breach here is the same breach `timeouts.analysis` names everywhere else. The default
    /// body ignores it for the same reason it ignores the file list — it does no work.
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
    /// [`BeginRunError::Failed`] for anything else. Both cancel the run, and the engine takes
    /// a different exit for each: `RunError::AnalysisTimeout`, which names `timeouts.analysis`,
    /// and `RunError::Provider`, which names the toolchain.
    fn begin_run(
        &self,
        files: &dyn Fn() -> Vec<FilePath>,
        budget: AnalysisBudget,
    ) -> Result<Vec<u8>, BeginRunError> {
        let _ = (files, budget);
        Ok(Vec::new())
    }

    /// The first error this provider produced, if it has produced one.
    ///
    /// **Why this is on the trait at all.** Every arm above answers "I don't know" — `None`,
    /// or `false` from [`Self::complete`] — when the provider is broken, because the trait has
    /// nowhere to put an error. So a killed sidecar is indistinguishable, at this boundary,
    /// from a compiler that simply has no type for that node: the file finishes *degraded* and
    /// is committed under a valid cache key, which is a limit degrading a run instead of
    /// cancelling it. This is the one way a caller can tell the two apart, so the engine asks
    /// after every file and cancels the run when it is `Some`.
    ///
    /// It is answered through the trait rather than off a concrete type because the engine
    /// holds an `Arc<dyn TypeProvider>` — a session's held provider (plan 6) included — and
    /// the alternative is a downcast, which is the door plan 4 closed for the run key.
    ///
    /// [`BeginRunError`] rather than an enum of its own: its two arms are exactly the two
    /// exits the engine has for a provider — a spent `timeouts.analysis`, and everything else
    /// — and a second type with the same two arms would be two spellings of one decision.
    ///
    /// The default is `None`: a provider with no out-of-band state has nothing to report, and
    /// the builtin one has no analogous failure.
    fn failure(&self) -> Option<BeginRunError> {
        None
    }

    /// Lines this provider wants said about the run it has just prepared.
    ///
    /// Asked once, after [`Self::begin_run`], and printed by the CLI on **stderr** — never on
    /// stdout, which carries a run's report and is what a machine reads. The engine prints
    /// nothing itself; it exposes these and the caller decides.
    ///
    /// What they are for is a decision a provider made silently that changes its answers. The
    /// `tsc` provider's is the ad-hoc program: a file no `tsconfig.json` *under the project
    /// root* claims is typed with this driver's own options rather than the project's, so
    /// `strict` is off for it and the same file checked from one directory up answers
    /// differently. That is not a failure — nothing is wrong and the run is correct — so it
    /// cannot be an error, and it is not nothing either.
    ///
    /// The default is empty: a provider that made no such decision has nothing to say, and a
    /// notice printed by every run is noise rather than information.
    fn notices(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether this provider is past repair and has to be built again.
    ///
    /// Asked by a session that holds a provider across requests
    /// (`crates/lanekeep-cli/src/session.rs`), before it hands the held one back. It is the
    /// difference between state that is *stale* and state that is *gone*: [`Self::revalidate`]
    /// repairs the first, and nothing repairs the second.
    ///
    /// The `tsc` provider is the one that answers `true`: a breached `timeouts.analysis` kills
    /// its sidecar, and nothing respawns it, so every later request in that session failed with
    /// "the sidecar exited without answering" for the life of the editor — one slow build
    /// bricking the session, where `lanekeep check` over the same project spawns a sidecar and
    /// succeeds. A limit must cancel the run it breached and nothing after it.
    ///
    /// The default is `false`: a provider whose state is ordinary memory cannot be gone while
    /// the process that holds it is running.
    fn needs_rebuild(&self) -> bool {
        false
    }

    /// Whether this provider spends the run's [`AnalysisBudget`].
    ///
    /// The engine reads the accumulator between one file and the next and cancels the run when
    /// it is past `timeouts.analysis`; this is what says whether that question is worth asking
    /// at all. It used to be asked of the *configuration* — `types.provider == 'tsc'` — which
    /// is the wrong thing twice over: a session that hands the engine a provider of its own
    /// (#191) is not described by the config it was built from, and a builtin run whose config
    /// happened to say `tsc` would be bounded by a budget that names work it never does.
    ///
    /// The default is `false`, which is the builtin oracle's answer: its cost is rule
    /// execution, and the run clock already bounds that.
    fn spends_analysis_budget(&self) -> bool {
        false
    }
}

/// Why a provider could not do its work — preparing a run in [`TypeProvider::begin_run`], or
/// answering a question afterwards, which [`TypeProvider::failure`] reports in the same shape.
///
/// The two arms are the engine's two exits: `Timeout` is `RunError::AnalysisTimeout`, which
/// names `timeouts.analysis`, and `Failed` is `RunError::Provider`, which names the toolchain.
/// Telling them apart matters because the remedies are different, and printing the wrong one
/// is advice that cannot work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginRunError {
    /// The provider's work outlived the analysis budget.
    Timeout(String),
    /// The provider could not do its work at all.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A provider that answers nothing and counts revalidations.
    ///
    /// Nothing here needs a parsed tree: the property under test is that the call reaches an
    /// implementor through the trait object a session holds, which is exactly what a
    /// concrete-typed test could not establish.
    #[derive(Default)]
    struct Counting(AtomicUsize);

    impl TypeProvider for Counting {
        fn type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn symbol_of(&self, _q: Query<'_>) -> Option<Symbol> {
            None
        }
        fn return_type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn is_assignable_to(&self, _q: Query<'_>, _module: &str, _name: &str) -> Option<bool> {
            None
        }
        fn complete(&self, _q: Query<'_>) -> bool {
            false
        }
        fn identity(&self) -> Vec<u8> {
            Vec::new()
        }
        fn revalidate(&self, _files: &FileAccess) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A provider that overrides nothing, to prove the default body exists.
    #[derive(Default)]
    struct Silent;

    impl TypeProvider for Silent {
        fn type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn symbol_of(&self, _q: Query<'_>) -> Option<Symbol> {
            None
        }
        fn return_type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn is_assignable_to(&self, _q: Query<'_>, _module: &str, _name: &str) -> Option<bool> {
            None
        }
        fn complete(&self, _q: Query<'_>) -> bool {
            false
        }
        fn identity(&self) -> Vec<u8> {
            Vec::new()
        }
    }

    #[test]
    fn revalidate_reaches_an_implementor_through_the_trait_object() {
        let counting = std::sync::Arc::new(Counting::default());
        let held: std::sync::Arc<dyn TypeProvider> = counting.clone();
        let files = FileAccess::new(std::path::Path::new("."));

        held.revalidate(&files);
        held.revalidate(&files);

        assert_eq!(
            counting.0.load(Ordering::Relaxed),
            2,
            "a session revalidates once per request, through the trait object it holds"
        );
    }

    #[test]
    fn a_provider_that_holds_nothing_need_not_override_revalidate() {
        // The default body is what keeps every other implementor compiling — a provider with
        // no cache has nothing to drop, and requiring it to say so would be noise.
        let held: std::sync::Arc<dyn TypeProvider> = std::sync::Arc::new(Silent);
        held.revalidate(&FileAccess::new(std::path::Path::new(".")));
    }
}
