//! The SDK every Rust-authored rule crate shares.
//!
//! Two things, deliberately, and nothing else: a name lookup over a query match
//! ([`Capture`]/[`capture`]), and a glob matcher ([`glob_matches`]). A macro that parses a
//! rule's query at compile time to generate a per-rule capture struct — recovering the
//! TypeScript SDK's `m.wildcard` dot-notation — is declined here rather than merely deferred:
//! two rules is not enough evidence to design that, and the cost of getting an authoring DSL
//! wrong is high. This is recorded so a third rule crate makes it a decision rather than a
//! reflex.
//!
//! This crate is not itself a WebAssembly component. It has no
//! `[package.metadata.component.target]` and generates no `bindings.rs` — a rule crate that
//! is a component depends on this one as a plain host-target library, the same way it would
//! depend on any other crate.
//!
//! # Why a trait, and not a type this crate defines
//!
//! An earlier version of this file gave `lanekeep-rule` its own `MatchEntry`, structurally
//! mirroring the WIT `match-entry` record, and asked a rule crate to convert its generated
//! `bindings::MatchEntry` into it field by field. That is a second Rust copy of a type this
//! crate does not own, and a copy can drift silently: a field added to the WIT record does
//! not break a struct-literal conversion, so nothing would notice the two moving apart — the
//! same trap `crates/lanekeep-wasm/tests/fixtures/engine-rule/Cargo.toml` names for pointing
//! at the engine's own `wit/` directory rather than a copy, one layer up.
//!
//! [`Capture`] asserts a *projection* instead — "has a name and a node" — so there is nothing
//! to drift and nothing to convert. A rule crate writes one
//! `impl lanekeep_rule::Capture for bindings::MatchEntry`: a foreign trait over a type local
//! to that crate (each component crate's generated `bindings` module is private to it), so
//! this is not an orphan-rule violation, and it costs one `impl` block rather than a
//! conversion at every `check`.

/// An opaque reference into the arena of the check-context that produced it.
///
/// Mirrors the WIT `node` alias (`crates/lanekeep-wasm/wit/world.wit`), which is itself a
/// plain `u32` rather than a resource — not a newtype, so this is interchangeable with
/// whatever a rule crate's own generated bindings call `Node`, with nothing to convert.
pub type Node = u32;

/// Something with the shape of one capture in a query match: the name the query gave it, and
/// the node it bound.
///
/// A rule crate implements this for its own generated `bindings::MatchEntry` — see the crate
/// documentation for why that is a trait implementation rather than a value this crate
/// converts on the rule crate's behalf.
pub trait Capture {
    /// The name the query gave this capture.
    fn name(&self) -> &str;
    /// The node it bound.
    fn node(&self) -> Node;
}

/// The node bound to `name` in `m`, or `None` if it did not participate in the match.
///
/// Absent rather than null: the WIT `match` doc comment records that a capture which did not
/// participate is simply missing from the list, so a linear search either finds it or it was
/// never there — there is no third state to represent. A query match is a handful of
/// captures at most, so the linear scan is not a cost worth an index to avoid. `m` takes a
/// slice rather than an iterator so a rule crate can call this as `capture(&m, "name")` and
/// let `Match`'s own `Deref<Target = [MatchEntry]>` (it is a plain `Vec`) do the rest.
pub fn capture<C: Capture>(m: &[C], name: &str) -> Option<Node> {
    m.iter()
        .find(|entry| entry.name() == name)
        .map(Capture::node)
}

/// Whether `c` is a line terminator under ECMAScript's definition: the four characters a
/// `RegExp`'s `.` does not match without the `s`/`dotAll` flag, which is how
/// `crates/lanekeep-rules/rules/no-unwrap.ts` and `no-glob-import.ts` build theirs.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Whether `value` matches the `*`-wildcard `pattern`, anchored at both ends.
///
/// Ports the `matches` helper duplicated in `crates/lanekeep-rules/rules/no-unwrap.ts` and
/// `no-glob-import.ts`. That original builds a `RegExp`, escaping every regex metacharacter
/// except `*` and then substituting `*` for `.*` — so everything but `*` is a literal, and
/// (carrying no `s`/`dotAll` flag) `*` does not span a line terminator. This is the same rule
/// without a regex engine: `pattern` is peeled into literal segments around its `*`s, and
/// `value` matches when it starts with the first segment, ends with the last, and contains
/// whatever segments sit between them, in that order, with no line terminator anywhere in a
/// gap a `*` had to span. A pattern with no `*` at all has nothing to span, so "anchored at
/// both ends" degenerates to an exact match.
///
/// Written with `split_once`/`rsplit_once`/`strip_prefix`/`strip_suffix` rather than manual
/// index arithmetic, so there is no slicing here that could panic on a boundary this function
/// got wrong.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let Some((first, rest)) = pattern.split_once('*') else {
        return value == pattern;
    };

    let Some(mut remaining) = value.strip_prefix(first) else {
        return false;
    };

    let last = if let Some((between, last)) = rest.rsplit_once('*') {
        for segment in between.split('*') {
            if segment.is_empty() {
                // Adjacent `*`s, or a run bordering the one already peeled off above —
                // either way, an empty literal is found at the current position for free,
                // with no gap to check.
                continue;
            }
            let Some((gap, after)) = remaining.split_once(segment) else {
                return false;
            };
            if gap.contains(is_line_terminator) {
                // The leftmost occurrence is the only one worth trying: its gap is a prefix
                // of every later occurrence's gap, so if this one already crosses a line
                // terminator, every later one crosses it too, and there is nothing left to
                // search for.
                return false;
            }
            remaining = after;
        }
        last
    } else {
        rest
    };

    let Some(gap) = remaining.strip_suffix(last) else {
        return false;
    };
    !gap.contains(is_line_terminator)
}

#[cfg(test)]
mod tests {
    use super::{Capture, Node, capture, glob_matches};

    struct Entry {
        name: &'static str,
        node: Node,
    }

    impl Capture for Entry {
        fn name(&self) -> &str {
            self.name
        }

        fn node(&self) -> Node {
            self.node
        }
    }

    #[test]
    fn a_pattern_is_anchored_at_both_ends() {
        assert!(!glob_matches("super", "super::*"));
        assert!(glob_matches("super::*", "super::*"));

        // The no-`*` case above is decided by a plain `==` and proves nothing about a
        // pattern that actually contains a `*`, which is every pattern the built-in rules'
        // `allow` lists use. Both ends, checked independently: a value with an extra
        // trailing byte after where the pattern's tail should land, and one with an extra
        // leading byte before where its head should start.
        assert!(!glob_matches("subject/*.rs", "subject/input.rs.bak"));
        assert!(!glob_matches("subject/*.rs", "xsubject/input.rs"));
    }

    #[test]
    fn a_star_spans_a_path_segment() {
        assert!(glob_matches("subject/*.rs", "subject/input.rs"));
    }

    #[test]
    fn a_star_does_not_span_a_line_terminator() {
        // The TypeScript original's `RegExp` carries no `s`/`dotAll` flag, so `.` — and by
        // extension `*` — does not match a line terminator. Reachable: `no-glob-import.ts`
        // defaults `allow` to `['*prelude*']` and reports at `ctx.text(m.wildcard)`, whose
        // `use_wildcard` node can legitimately wrap onto a second line —
        // `use std::\n    prelude::*;` — where the TypeScript original does not match and
        // reports a violation. `\r\n` closes the same gap on Windows line endings.
        assert!(!glob_matches("*prelude*", "std::\n    prelude::*"));
        assert!(!glob_matches("*prelude*", "std::\r\n    prelude::*"));
        assert!(glob_matches("*prelude*", "std::prelude::*"));
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
        let entries = vec![Entry {
            name: "call",
            node: 7,
        }];
        assert_eq!(capture(&entries, "call"), Some(7));
        assert_eq!(capture(&entries, "method"), None);
    }
}
