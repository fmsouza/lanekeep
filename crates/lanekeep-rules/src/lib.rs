//! Built-in rules shipped with lanekeep.
//!
//! The rules shipping with lanekeep, authored in TypeScript against the same host API that
//! project-authored rules use, embedded into the binary at build time.
//!
//! Built-ins deliberately get no privileged path into the engine. Rules dogfooding the
//! public API is the strongest available evidence that the API is sufficient for real work.
