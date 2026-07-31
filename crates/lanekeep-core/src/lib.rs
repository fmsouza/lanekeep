//! Core types and execution engine for lanekeep.
//!
//! File walking, query evaluation, the facts pipeline, violations, and the `Rule` trait.
//!
//! This crate owns the contract every other crate is written against. `Rule` is treated as
//! public API that happens not to be published: no built-in rule may reach past it into
//! walker internals or cache state, because that boundary is what keeps future rule sources
//! additive. See `docs/architecture.md` §14.
