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
//!
//! Tracked, confined file reads (`FileAccess`) live here too, alongside `tracked` rather
//! than inside whichever engine happened to need them first — every engine that runs a rule
//! needs the identical confinement and tracking rules, and a copy per engine is exactly the
//! kind of drift lanekeep's own self-check rules exist to catch elsewhere.
//!
//! So do the execution budgets (`Limits`, `RunClock`) and their enforcement (`Budget`,
//! `Trip`), for a related but sharper reason: there is exactly one global run budget, not
//! one per engine, so two independent `RunClock`s would each be correct in isolation while
//! the run as a whole overran both. See [`limits`] for why that failure needs no maintenance
//! drift to happen — unlike the per-engine-instance types above, it is wrong the moment a
//! second copy exists at all.

pub mod card;
pub mod changed;
pub mod discovery;
pub mod fact;
pub mod files;
pub mod fix;
pub mod gates;
pub mod limits;
pub mod location;
pub mod query_cover;
pub mod rule_id;
pub mod severity;
pub mod suppression;
pub mod tracked;
pub mod violation;

pub use card::{CardProblem, Examples, RuleCard};
pub use changed::ChangeError;
pub use discovery::{Discovery, DiscoveryError};
pub use fact::Fact;
pub use files::{FileAccess, ReadError};
pub use fix::Fix;
pub use gates::{CompiledGates, GateError, Gates};
pub use limits::{
    DEFAULT_GLOBAL_TIMEOUT, DEFAULT_MEMORY_BYTES, DEFAULT_RULE_TIMEOUT, Limits, RunClock,
};
pub use location::{FilePath, Location, Position};
pub use rule_id::{Namespace, ParseRuleIdError, RuleId};
pub use severity::{ParseSeverityError, Severity};
pub use suppression::{Suppression, Suppressions};
pub use tracked::{ContentHash, TrackedRead};
pub use violation::{Violation, any_failing, sort};

/// A host analysis a rule can declare it needs.
///
/// Closed, and small on purpose. A capability exists here only once something implements it
/// or refuses it by name — the alternative is a rule declaring a dependency on an analysis
/// nothing will ever provide, which reads as configuration rather than as the error it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Type answers about a node — `ctx.types`.
    Types,
    /// Dataflow answers about a value's movement. Declared, not yet implemented.
    Dataflow,
}

impl Capability {
    /// The name a rule writes, which is the name the refusal prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Types => "types",
            Self::Dataflow => "dataflow",
        }
    }

    /// The capability that name denotes, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "types" => Some(Self::Types),
            "dataflow" => Some(Self::Dataflow),
            _ => None,
        }
    }

    /// Every capability, in the order `as_str` and `parse` agree on.
    ///
    /// For a refusal that lists what a rule may name — mirrors [`Namespace::built_ins`] for
    /// the same reason: one place naming every variant, so a message enumerating them cannot
    /// name a different set than `parse` recognizes.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Types, Self::Dataflow]
    }
}
