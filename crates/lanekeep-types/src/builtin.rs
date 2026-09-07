//! The bounded provider: this crate's own oracle, plus the files it is allowed to open.
//!
//! Run-scoped state lives here rather than on [`TypeScriptOracle`](crate::TypeScriptOracle),
//! which owns exactly one parse and must stay that way. What this holds is a parser, a cache
//! of parsed declaration files and the memo of paths that were not there — so a library's
//! `.d.ts` is parsed once per run whatever imports it, and a miss is not re-probed per query.
//!
//! # Locks
//!
//! Every one is uncontended in the ordinary case and none is held across a call that can
//! block on anything but the filesystem. Poisoning is treated as "take the value anyway", the
//! posture `lanekeep_core::files::FileAccess` documents on its own memo: nothing under these
//! locks can panic, and refusing to answer because an unrelated worker died would turn a
//! rule's question into a failure with nothing to do with it.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lanekeep_lang::Language;

use crate::oracle::{TypeScriptOracle, TypeScriptSupport};
use crate::provider::{Query, TypeProvider};
use crate::types::{Symbol, Type};

/// The provider that reads declaration files with this crate's own oracle.
pub struct BuiltinProvider {
    support: TypeScriptSupport,
    /// One parser for the run's declaration files, behind a lock.
    ///
    /// Not a second parse of any file the engine already parsed: this opens `node_modules`
    /// and sibling declaration files, which are not in the corpus and have no shared tree.
    ///
    parser: Mutex<tree_sitter::Parser>,
}

impl fmt::Debug for BuiltinProvider {
    /// Hand-written because neither `TypeScriptSupport` nor `tree_sitter::Parser` is
    /// `Debug`, the same reason and the same shape as the oracle's own impl.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinProvider").finish_non_exhaustive()
    }
}

impl BuiltinProvider {
    /// Confirm a grammar speaks TypeScript and build a provider over it.
    ///
    /// `None` on the same two conditions [`TypeScriptSupport::probe`] refuses on — a grammar
    /// without the vocabulary this oracle reads, or a language with no binding resolver —
    /// plus a third: a grammar the parser will not accept at all. Each would otherwise
    /// produce confident nonsense rather than an error.
    ///
    /// The only constructor. A provider must *parse* declaration files, so it cannot be
    /// built from a resolver alone.
    #[must_use]
    pub fn probe(language: &dyn Language) -> Option<Self> {
        let support = TypeScriptSupport::probe(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        Some(Self {
            support,
            parser: Mutex::new(parser),
        })
    }

    /// The parser, whether or not another thread died holding it.
    #[expect(dead_code, reason = "called starting in Task 7's declaration walk")]
    fn parser(&self) -> MutexGuard<'_, tree_sitter::Parser> {
        self.parser.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An oracle over the file a question is about.
    fn oracle<'q>(&self, q: &Query<'q>) -> TypeScriptOracle<'q> {
        TypeScriptOracle::new(&self.support, q.tree, q.source)
    }
}

impl TypeProvider for BuiltinProvider {
    fn type_of(&self, q: Query<'_>) -> Option<Type> {
        self.oracle(&q).type_of(q.node)
    }

    fn symbol_of(&self, q: Query<'_>) -> Option<Symbol> {
        self.oracle(&q).symbol_of(q.node)
    }

    /// Nothing yet — Task 12 fills this in.
    ///
    /// `None` rather than a guess for the same reason every other arm answers `None`: an
    /// unimplemented arm and an unanswerable question are the same thing to a rule, and both
    /// are silence rather than a wrong report.
    fn return_type_of(&self, _q: Query<'_>) -> Option<Type> {
        None
    }

    /// Nothing yet — Task 13 fills this in. See [`Self::return_type_of`].
    fn is_assignable_to(&self, _q: Query<'_>, _module: &str, _name: &str) -> Option<bool> {
        None
    }

    /// Nothing crosses a file boundary yet, so nothing can make an answer partial.
    ///
    /// Task 14 replaces this with the eager import pass.
    fn complete(&self, _q: Query<'_>) -> bool {
        true
    }

    fn identity(&self) -> Vec<u8> {
        // Tagged as well as hashed. `oracle_identity` alone would let a future provider that
        // happened to derive its identity the same way collide with this one, and the tag is
        // what makes "which provider answered" part of the key rather than an inference.
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(b"builtin:");
        out.extend_from_slice(&crate::oracle_identity());
        out
    }
}

/// `BuiltinProvider` is shareable, checked at compile time rather than believed.
///
/// The engine holds one in an `Arc` that rayon moves between workers, so a field that is not
/// `Send + Sync` must stop the build here rather than as an unsatisfied bound two crates away
/// — the reasoning `FileAccess`'s own `assert_shareable` block gives.
const _: () = {
    const fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<BuiltinProvider>();
};
