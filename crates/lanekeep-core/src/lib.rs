//! Core types and execution engine for lanekeep.
//!
//! File walking, query evaluation, the facts pipeline, violations, and the `Rule` trait.
//!
//! This crate owns the contract every other crate is written against. `Rule` is treated as
//! public API that happens not to be published: no built-in rule may reach past it into
//! walker internals or cache state, because that boundary is what keeps future rule sources
//! additive. See `docs/architecture.md` §14.
//!
//! # What is here so far
//!
//! Rule identity, severity, source locations, rule cards, violations with their canonical
//! ordering, and the facts a per-file pass hands to the reduce phase.
//!
//! These types are foundational in a specific sense: they are what appears in JSON output,
//! in cache entries, and in suppression comments users type by hand. Getting them wrong is
//! expensive in a way that getting the walker wrong is not, because only these are visible
//! from outside.

pub mod card;
pub mod discovery;
pub mod fact;
pub mod gates;
pub mod location;
pub mod rule_id;
pub mod severity;
pub mod violation;

pub use card::{CardProblem, Examples, RuleCard};
pub use discovery::{Discovery, DiscoveryError};
pub use fact::Fact;
pub use gates::{CompiledGates, GateError, Gates};
pub use location::{FilePath, Location, Position};
pub use rule_id::{Namespace, ParseRuleIdError, RuleId};
pub use severity::{ParseSeverityError, Severity};
pub use violation::{Violation, any_failing, sort};
