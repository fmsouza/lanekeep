//! The SDK every Rust-authored rule crate shares.
//!
//! Two things, deliberately, and nothing else: a name lookup over a query match
//! ([`Captures`]), and a glob matcher ([`glob_matches`]). A macro that parses a rule's query
//! at compile time to generate a per-rule capture struct — recovering the TypeScript SDK's
//! `m.wildcard` dot-notation — is declined here rather than merely deferred: two rules is
//! not enough evidence to design that, and the cost of getting an authoring DSL wrong is
//! high. This is recorded so a third rule crate makes it a decision rather than a reflex.
//!
//! This crate is not itself a WebAssembly component. It has no
//! `[package.metadata.component.target]` and generates no `bindings.rs` — a rule crate that
//! is a component depends on this one as a plain host-target library, the same way it would
//! depend on any other crate. `Node` and [`MatchEntry`] mirror the WIT `node` alias and
//! `match-entry` record (`crates/lanekeep-wasm/wit/world.wit`) field for field, so converting
//! a rule crate's own generated `bindings::MatchEntry` into one of these is a one-line,
//! per-field copy at the call site rather than something this crate can do on a rule crate's
//! behalf — `lanekeep-rule` is built before any component crate that would depend on it, so
//! it cannot name a bindings type that does not exist yet, and each component crate
//! generates its own bindings privately, so no two rule crates' `bindings::MatchEntry`
//! are the same Rust type even though they are structurally identical.

/// An opaque reference into the arena of the check-context that produced it.
///
/// Mirrors the WIT `node` alias (`crates/lanekeep-wasm/wit/world.wit`), which is itself a
/// plain `u32` rather than a resource — not a newtype, so this is interchangeable with
/// whatever a rule crate's own generated bindings call `Node`, with nothing to convert.
pub type Node = u32;

/// One capture of a query match: the name the query gave it, and the node it bound.
///
/// Mirrors the WIT `match-entry` record field for field. See the crate documentation for why
/// a rule crate builds these from its own generated `bindings::MatchEntry` by hand instead of
/// this crate converting on its behalf.
#[derive(Debug)]
pub struct MatchEntry {
    /// The name the query gave this capture.
    pub name: String,
    /// The node it bound.
    pub node: Node,
}

/// A query match's captures, looked up by the name the query gave them.
///
/// Wraps the generated `Match` (`crates/lanekeep-wasm/wit/world.wit`'s `match-entry`/`match`
/// — a plain `Vec<MatchEntry>`, not a resource) rather than indexing into a map: a query
/// match is a handful of captures at most, and a linear scan over a handful of entries is not
/// a cost worth a second allocation to avoid.
#[derive(Debug)]
pub struct Captures(Vec<MatchEntry>);

impl From<Vec<MatchEntry>> for Captures {
    fn from(entries: Vec<MatchEntry>) -> Self {
        Self(entries)
    }
}

impl Captures {
    /// The node bound to `name`, or `None` if it did not participate in the match.
    ///
    /// Absent rather than null: the WIT `match` doc comment records that a capture which did
    /// not participate is simply missing from the list, so a linear search either finds it or
    /// it was never there — there is no third state to represent.
    pub fn get(&self, name: &str) -> Option<Node> {
        self.0
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.node)
    }
}

/// Whether `value` matches the `*`-wildcard `pattern`, anchored at both ends.
///
/// Ports the `matches` helper duplicated in `crates/lanekeep-rules/rules/no-unwrap.ts` and
/// `no-glob-import.ts`. That original builds a `RegExp`, escaping every regex metacharacter
/// except `*` and then substituting `*` for `.*` — so everything but `*` is a literal. This
/// is the same rule without a regex engine: `pattern` is peeled into literal segments around
/// its `*`s, and `value` matches when it starts with the first segment, ends with the last,
/// and contains whatever segments sit between them, in that order. A pattern with no `*` at
/// all has nothing to span, so "anchored at both ends" degenerates to an exact match.
///
/// Written with `split_once`/`rsplit_once`/`strip_prefix` rather than manual index
/// arithmetic, so there is no slicing here that could panic on a boundary this function got
/// wrong.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let Some((first, rest)) = pattern.split_once('*') else {
        return value == pattern;
    };

    let Some(mut remaining) = value.strip_prefix(first) else {
        return false;
    };

    // Peel the last segment off the far end of what is left, so the loop below only ever
    // has to walk literals that sit strictly between two `*`s.
    let last = if let Some((between, last)) = rest.rsplit_once('*') {
        for segment in between.split('*') {
            if segment.is_empty() {
                // Adjacent `*`s, or a run bordering the one already peeled off above —
                // either way, an empty literal is found at the current position for free.
                continue;
            }
            let Some((_, after)) = remaining.split_once(segment) else {
                return false;
            };
            remaining = after;
        }
        last
    } else {
        rest
    };

    remaining.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::{Captures, MatchEntry, Node, glob_matches};

    fn entry(name: &str, node: Node) -> MatchEntry {
        MatchEntry {
            name: name.to_owned(),
            node,
        }
    }

    #[test]
    fn a_pattern_is_anchored_at_both_ends() {
        assert!(!glob_matches("super", "super::*"));
        assert!(glob_matches("super::*", "super::*"));
    }

    #[test]
    fn a_star_spans_a_path_segment() {
        assert!(glob_matches("subject/*.rs", "subject/input.rs"));
    }

    #[test]
    fn a_regex_metacharacter_in_a_pattern_is_a_literal() {
        // The TypeScript original escapes these before building a RegExp. A Rust
        // implementation that forgot would make `a.rs` match `axrs`.
        assert!(!glob_matches("a.rs", "axrs"));
        assert!(glob_matches("a.rs", "a.rs"));
    }

    #[test]
    fn a_capture_that_did_not_participate_is_absent_rather_than_null() {
        let captures = Captures::from(vec![entry("call", 7)]);
        assert_eq!(captures.get("call"), Some(7));
        assert_eq!(captures.get("method"), None);
    }
}
