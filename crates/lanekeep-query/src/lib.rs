//! tree-sitter query parsing and compilation for lanekeep.
//!
//! Parses and compiles the tree-sitter queries that gate rule execution.
//!
//! Queries are the gate, not the rule language: they select which nodes reach a
//! Turing-complete TypeScript handler. That is what keeps JavaScript execution proportional
//! to matches rather than to nodes.
