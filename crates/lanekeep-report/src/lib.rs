//! Output formatting for lanekeep.
//!
//! The human, JSON, SARIF and agent reporters.
//!
//! Violations are always sorted by `(ruleId, file, line, column)`. Determinism matters more
//! than usual here: an agent reads this output twice and must not see reordering as change.
