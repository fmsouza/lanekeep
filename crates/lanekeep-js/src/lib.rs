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
//! `FileAccess`/`ReadError` and `NodeArena`/`Handle` are re-exported rather than owned here:
//! the first lives in `lanekeep-core`, the second in `lanekeep-nodes`. Both moved out once a
//! second engine (`lanekeep-wasm`) needed the identical definitions — a copy per engine
//! would let one run enforce two different notions of "the same file" or "the same node",
//! each correct alone and disagreeing with the other.
//!
//! `Limits`/`RunClock` (and the `Budget`/`Trip` this sandbox arms and reads) moved to
//! `lanekeep-core` for a sharper version of the same reason: a run has exactly one global
//! budget, not one per engine, so two `RunClock`s would each be a correct clock in isolation
//! while the run as a whole overran both. This sandbox still does the arming, disarming and
//! interrupt wiring — only the type that makes "one clock" possible moved out.
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
pub mod loader;
pub mod sandbox;
pub mod typescript;

pub use error::SandboxError;
pub use host::{
    EmittedFact, HOST_API_VERSION, HostContext, ReduceContext, ReduceFact, ReduceReport, Report,
    merge_file,
};
pub use lanekeep_core::files::{FileAccess, ReadError};
pub use lanekeep_core::limits::{
    DEFAULT_GLOBAL_TIMEOUT, DEFAULT_MEMORY_BYTES, DEFAULT_RULE_TIMEOUT, Limits, RunClock,
};
/// Re-exported so consumers can supply languages without depending on `lanekeep-lang` directly.
pub use lanekeep_lang::Language;
pub use lanekeep_nodes::{Handle, NodeArena};
pub use loader::{
    BuiltinComponent, BuiltinSource, HOST_MODULE, ResolveError, RuleLoader, RuleResolver, RuleRoot,
};
pub use sandbox::Sandbox;
pub use typescript::{StripError, Unsupported, strip_types};
