//! The node arena: parsed-tree handles shared across lanekeep's rule-execution engines.
//!
//! [`NodeArena`] and [`Handle`] are how a parsed file's nodes cross into a rule, as opaque
//! integer handles rather than materialized objects (`docs/architecture.md` §14). That
//! invariant is not specific to any one engine, and neither is the arena: it is built on
//! `lanekeep-query` and `lanekeep-lang` alone, with no engine-specific dependency at all.
//!
//! # Why this is its own crate rather than living in `lanekeep-js`
//!
//! It used to. `lanekeep-js`'s QuickJS sandbox is still the only engine that runs a rule
//! today, but `lanekeep-wasm`'s component runtime is expected to become a second one before
//! the first is retired, and both would need this exact type: what a node is, and how a
//! handle resolves to one, cannot differ between them without a single run being able to
//! disagree with itself about whether two handles name the same node. Defining it once,
//! here, is what keeps that question from being askable — the alternative, a copy per
//! engine, is exactly the hand-maintained duplication lanekeep's own self-check rules exist
//! to catch elsewhere.
//!
//! `lanekeep-core` was the first place this was tried; it does not fit, because
//! `lanekeep-query` — which the arena needs to run a query against a subtree — already
//! depends on `lanekeep-core`. A dependency back from core would be a cycle, not a move.
//!
//! See [`nodes`] for the arena itself, including why it stores paths from the root rather
//! than borrowed `tree_sitter::Node` references.

pub mod nodes;

pub use nodes::{Handle, NodeArena};
