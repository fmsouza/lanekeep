//! Embedded JavaScript sandbox and host API for lanekeep rules.
//!
//! The embedded QuickJS runtime, the capability-restricted host API, the TypeScript
//! stripping step, and the module loader.
//!
//! The sandbox boundary lives here. Rule code reaches exactly the functions this crate
//! exposes and nothing else: no ambient filesystem, no process, no network, no clock, no
//! randomness. Those globals are not restricted, they are absent.
//!
//! Every addition to the host API widens the trust boundary and bumps the API version that
//! feeds the cache key.
//!
//! # How absence is achieved
//!
//! Two mechanisms, and the first is much stronger than the second.
//!
//! **Not installed.** The engine's optional intrinsics are opted into rather than opted out
//! of, so `Date`, `Performance` and `WeakRef` are never created. There is no original for a
//! rule to reach: nothing to patch, nothing to restore, no prototype chain leading back.
//!
//! **Deleted at startup.** `Math.random` lives among the non-optional base objects, so it
//! has to go afterwards. This is weaker in principle — deletion can be undone if a
//! reference escapes — but a rule that defines its own `Math.random` has written
//! deterministic code, which is all this needs to guarantee.
//!
//! Anything a host function does not offer, a rule cannot do. `fs`, `process`, `fetch`,
//! `setTimeout` and friends were never part of this engine to begin with, which is asserted
//! rather than assumed.
//!
//! # What is here so far
//!
//! The sandbox and its budgets. The host API, TypeScript stripping and the module loader
//! arrive in later milestones.

pub mod error;
pub mod host;
pub mod limits;
pub mod loader;
pub mod nodes;
pub mod sandbox;
pub mod typescript;

pub use error::SandboxError;
pub use host::{HostContext, Report};
pub use limits::{
    DEFAULT_GLOBAL_TIMEOUT, DEFAULT_MEMORY_BYTES, DEFAULT_RULE_TIMEOUT, Limits, RunClock,
};
pub use loader::{HOST_MODULE, ResolveError, RuleLoader, RuleResolver, RuleRoot};
pub use nodes::{Handle, NodeArena};
pub use sandbox::Sandbox;
pub use typescript::{StripError, Unsupported, strip_types};
