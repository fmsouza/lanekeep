//! Configuration loading and canonicalized hashing for lanekeep.
//!
//! Loads `lanekeep.config.ts`, resolves the rule graph, and derives the hashes feeding the
//! cache key.
//!
//! Hashing is the subtle part. `ruleset_hash` must cover every module in the rule import
//! graph rather than just the entry points, or a change to a shared helper silently serves
//! stale results.
