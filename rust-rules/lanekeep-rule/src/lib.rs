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

/// Declare the rules a component hosts.
///
/// The index a rule answers to is its position here, and a rule crate never spells one:
/// an index that has to be kept in step with a list by hand is a mismatch waiting to
/// happen, and the mismatch is silent — a rule would simply answer to the wrong config.
///
/// ```ignore
/// lanekeep_rule::ruleset! {
///     "lanekeep/no-unwrap" => NoUnwrap,
/// }
/// ```
///
/// # What it expands to, and what it expects
///
/// A `trait Rule`, a `const RULE_IDS`, a `struct Component`, and the `impl Guest for Component`
/// that dispatches every export of `crates/lanekeep-wasm/wit/world.wit`'s `rule` world to one of
/// the types named above. The crate keeps its own `bindings::export!(Component ...)`, because
/// that macro reaches into the generated bindings' internal shims and the `unsafe` it needs is
/// allowed at that one site rather than wherever this is written.
///
/// Each `$rule` type is a plain unit struct implementing that trait, whose methods are the
/// world's minus the index:
///
/// ```text
/// impl Rule for NoUnwrap {
///     fn metadata() -> RuleMetadata { … }
///     fn configure(options_json: String) -> Result<(), String> { … }
///     fn has_check() -> bool { … }
///     fn has_reduce() -> bool { … }
///     fn check(ctx: &CheckContext, m: Match) -> Result<(), RuleError> { … }
///     fn reduce(ctx: &ReduceContext) -> Result<(), RuleError> { … }
/// }
/// ```
///
/// **The trait is generated rather than declared in this crate, and it has to be.** Every type in
/// those signatures — `RuleMetadata`, `CheckContext`, `Match`, `RuleError` — belongs to the *rule
/// crate's* own generated `bindings` module, which is private to it, so this crate cannot name
/// one. A `macro_rules!` expansion resolves free identifiers at the call site, which is why the
/// expansion can name them and this crate's source cannot. It is the same constraint [`Capture`]
/// exists for, one layer up.
///
/// **A trait rather than inherent associated functions, which was the first shape and did not
/// survive clippy.** The world dictates these signatures: `check` takes its `Match` by value
/// because the canonical ABI hands one over, and both handlers return a `Result` because the
/// world gives them an error channel a Rust rule has no use for. As *inherent* functions that
/// reads as a mistake and `needless_pass_by_value` and `unnecessary_wraps` say so, five errors per
/// rule crate. Clippy exempts a trait implementation from both, because there the signature is
/// not the author's to choose — which is exactly the situation. The alternative was an `#[expect]`
/// per rule for a lint that is right about every other function in the crate.
///
/// # Dispatch goes through the id rather than through a counted index
///
/// `macro_rules!` has no way to count its repetitions, and the arrangements that fake one —
/// a mutable counter incremented per arm, a recursive helper threading an accumulator — put the
/// index and the rule in two places that have to agree. Matching on `RULE_IDS[index]` instead
/// makes the list itself the mapping: `rules` reports `RULE_IDS`, and every other export looks
/// the index back up in it, so the enumeration a host reads and the dispatch it then drives
/// cannot disagree.
#[macro_export]
macro_rules! ruleset {
    ($($id:literal => $rule:ty),+ $(,)?) => {
        /// One rule, as its author writes it: the world's exports minus the index the
        /// component dispatches on.
        trait Rule {
            /// What this rule is. Read once, at prepare time, after it has been configured.
            fn metadata() -> RuleMetadata;
            /// This rule's options as JSON, or `null` when it was named with none.
            fn configure(options_json: String) -> Result<(), String>;
            /// Whether this rule has a per-file pass.
            fn has_check() -> bool;
            /// Whether this rule has a cross-file pass.
            fn has_reduce() -> bool;
            /// The per-file pass, for one query match.
            fn check(ctx: &CheckContext, m: Match) -> Result<(), RuleError>;
            /// The cross-file pass, once per run.
            fn reduce(ctx: &ReduceContext) -> Result<(), RuleError>;
        }

        /// Every rule this component hosts, by id, in the order `rules` reports them.
        const RULE_IDS: &[&str] = &[$($id),+];

        /// The component this crate builds.
        struct Component;

        impl Guest for Component {
            fn rules() -> Vec<String> {
                RULE_IDS.iter().map(|id| (*id).to_owned()).collect()
            }

            fn configure(rule: u32, options_json: String) -> Result<(), String> {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::configure(options_json),)+
                    _ => $crate::no_such_rule(rule),
                }
            }

            fn metadata(rule: u32) -> RuleMetadata {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::metadata(),)+
                    _ => $crate::no_such_rule(rule),
                }
            }

            fn has_check(rule: u32) -> bool {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::has_check(),)+
                    _ => $crate::no_such_rule(rule),
                }
            }

            fn has_reduce(rule: u32) -> bool {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::has_reduce(),)+
                    _ => $crate::no_such_rule(rule),
                }
            }

            fn check(rule: u32, ctx: &CheckContext, m: Match) -> Result<(), RuleError> {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::check(ctx, m),)+
                    _ => $crate::no_such_rule(rule),
                }
            }

            fn reduce(rule: u32, ctx: &ReduceContext) -> Result<(), RuleError> {
                match $crate::rule_id(RULE_IDS, rule) {
                    $(Some($id) => <$rule as Rule>::reduce(ctx),)+
                    _ => $crate::no_such_rule(rule),
                }
            }
        }
    };
}

/// Build the `queries` list of a `RuleMetadata`, one `QueryFor` per language.
///
/// The convenience for the common case: a rule targets one grammar and has one query, so
/// `metadata` stays one line instead of a hand-written `vec![QueryFor { ... }]`. A rule that
/// targets several grammars writes the list out by hand, since each grammar has its own query.
///
/// ```ignore
/// queries: lanekeep_rule::queries!["rust" => "(call_expression) @call"],
/// ```
///
/// `QueryFor` resolves in the rule crate's own generated `bindings` module, exactly as
/// `RuleMetadata` does — which is why this is a macro and not a function.
#[macro_export]
macro_rules! queries {
    ($($language:literal => $query:expr),+ $(,)?) => {
        vec![$(QueryFor { language: $language.to_owned(), query: $query.to_owned() }),+]
    };
}

/// The id at `index`, or `None` when the component hosts no such rule.
///
/// Called by [`ruleset!`]'s expansion and by nothing a rule author writes. It is a function
/// rather than an indexing expression inside the macro because indexing panics and this
/// answers instead — the refusal belongs to [`no_such_rule`], where it says what went wrong.
#[must_use]
pub fn rule_id<'a>(ids: &[&'a str], index: u32) -> Option<&'a str> {
    usize::try_from(index)
        .ok()
        .and_then(|index| ids.get(index))
        .copied()
}

/// Refuse an index that names no rule this component hosts.
///
/// **It traps, which is the honest answer and not a shortcut.** Three of the world's exports —
/// `metadata`, `has-check`, `has-reduce` — have no error channel, so there is nowhere to put a
/// refusal, and the alternatives are to invent one rule's answer for another rule's index or to
/// return a plausible-looking blank. Both would be a host and a component silently disagreeing
/// about which rule is running, which is the failure the whole `rules`-then-index arrangement
/// exists to make impossible.
///
/// It is not reachable from a host that enumerates first: `rules` reports exactly the indices
/// that answer. Reaching it means the host asked about a rule this component never listed.
///
/// # Panics
///
/// Always. In a component a panic is a trap, which is how a guest with no channel says no.
#[cold]
#[expect(
    clippy::panic,
    reason = "three of the world's exports have no error channel, and a wrong index must not \
              be answered with another rule's data"
)]
pub fn no_such_rule(index: u32) -> ! {
    panic!(
        "no rule at index {index}: this component's `rules` export lists every index that \
         answers, and this is not one of them"
    );
}

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
/// `RegExp`'s `.` does not match without the `s`/`dotAll` flag, which is how the TypeScript
/// rules these two were ported from built theirs — see [`glob_matches`].
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Whether `value` matches the `*`-wildcard `pattern`, anchored at both ends.
///
/// Ports the `matches` helper that was duplicated in
/// `crates/lanekeep-rules/rules/no-unwrap.ts` and `no-glob-import.ts` — both deleted when their
/// components became the shipped rules, and recoverable with
/// `git log --diff-filter=D -- <path>`. That original builds a `RegExp`, escaping every metacharacter
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
        // extension `*` — does not match a line terminator. Reachable: `no-glob-import`
        // defaults `allow` to `['*prelude*']` and reports at the wildcard's text, whose
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
