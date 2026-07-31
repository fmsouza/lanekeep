//! Language trait and registry for lanekeep.
//!
//! The `Language` trait and the registry mapping file extensions onto grammars and binding
//! resolvers.
//!
//! This abstraction exists before it has a second implementor on purpose. Retrofitting it
//! after a second language arrives is the expensive version of the same work.
