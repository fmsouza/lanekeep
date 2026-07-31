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
