//! The type oracle: what a node's type is, answered from one parsed file.
//!
//! This is the "bounded oracle" of the type-aware rules design. It answers from the file in
//! front of it and nothing else — no `tsconfig.json`, no declaration files, no compiler —
//! and it answers `None` whenever it cannot be sure. That is the whole contract: a rule
//! choosing to be silent on `None` is sound, and one choosing to report on it is the
//! author's decision rather than the engine's.
//!
//! # Why this is its own crate
//!
//! It reaches declarations through [`lanekeep_lang::binding::BindingResolver::declaration_of`],
//! a language-neutral question, so it never needs a language crate's internals. Only its
//! knowledge of TypeScript *syntax* is language-specific, and that is held in one type whose
//! constructor refuses a grammar that does not speak it.
//!
//! # What is deliberately absent
//!
//! No clock, no environment, no randomness, no `HashMap` iteration. A cached result computed
//! by this oracle must still be valid, so nothing here may observe anything outside the
//! bytes it was handed.

mod oracle;
mod table;
mod types;

pub use oracle::{TypeScriptOracle, TypeScriptSupport};
pub use types::{Primitive, Symbol, Type};

/// What this oracle *is*, as a digest of every source file that decides an answer.
///
/// A cache-key input for whoever wires the oracle up: a result computed by an oracle that no
/// longer exists must not be served. It is derived rather than hand-maintained, because the
/// hand-maintained alternative is `lanekeep_js::HOST_API_VERSION`, whose own documentation
/// says plainly that nothing detects a missed bump.
///
/// **Not a hash of the tables.** The `+` arm, the shadow check on builtin calls and the
/// recursion bound are logic rather than table rows, and an oracle whose `+` arm is corrected
/// gives different answers from an identical table. Hashing the data alone would
/// under-invalidate on exactly the changes most likely to matter, which is the asymmetric
/// failure the cache key exists to prevent.
///
/// This over-invalidates instead: editing a comment in this crate discards every cached
/// type-aware result. That is the trade `hash_ruleset` already makes for rule source, and
/// for the same reason — over-invalidation costs a recompute, under-invalidation reports a
/// wrong answer and gives no sign that it did.
///
/// The grammar is not folded in here. A run's cache key already carries a structural digest of
/// every registered grammar, so a TypeScript grammar bump invalidates through that term;
/// hashing it twice would be one place too many.
#[must_use]
pub fn oracle_identity() -> [u8; 32] {
    // Written by `build.rs`, which walks `src/` so that a file added but not listed cannot
    // be a silent gap.
    lanekeep_lang::decode_hex32(env!("LANEKEEP_TYPES_ORACLE_HASH"))
}
