//! Content-addressed result cache with dependency tracking for lanekeep.
//!
//! A memory-mapped, content-addressed store holding violations, facts, suppressions and the
//! tracked read dependencies of each file.
//!
//! The cache is disposable by design: any read error means a cold recompute, never a
//! failure. That is what makes a purpose-built on-disk format acceptable rather than
//! reckless.
