//! The two engines refuse a broken `ctx.report` in the same words.
//!
//! `packages/lanekeep/runtime/host.js` translates the world's `option`s, `result`s and capture
//! lists into the API `index.d.ts` publishes — and `crates/lanekeep-js/src/host.rs` is the
//! specification both engines follow, its own header says so. That makes the refusal prose a
//! duplicate kept honest by hand: a message is edited when the behavior behind it is edited,
//! so prose that no longer matches on one side is the cheapest available signal that the other
//! side has moved and this one has not. This file is that check, on the model of
//! `resolver_parity.rs` (which covers the resolver's two halves the same way).
//!
//! **Fragments, not whole messages.** Each side wraps its strings for its own formatter, and a
//! fragment that fits inside one wrapped line on both sides is the comparison that survives a
//! reflow. The runtime strings are the claim — byte-identical — and a fragment is the canary
//! chosen for being easy to watch.
//!
//! What is deliberately not here: the per-file options-object refusal, whose two engines
//! already answer with their own wording for their own phase (the per-file options take a
//! `fix`, and the reduce ones refuse it — different methods, different worlds), and anything
//! `lanekeep-wasm`'s component host says, which the fixtures already drive.

/// The engine's host, read at compile time so this cannot be pointed at a stale copy.
const HOST_RS: &str = include_str!("../src/host.rs");

/// The component runtime, read at compile time for the same reason.
const HOST_JS: &str = include_str!("../../../packages/lanekeep/runtime/host.js");

/// The reduce-report refusal prose both engines must carry, fragment by fragment.
///
/// The position refusal and the `at` shape are the world's own words (`world.wit` requires a
/// location and takes no node), so a rule's mistake has to name the same thing whichever
/// engine runs it; the fix refusal is the one #238 added, and the message refusal is the
/// strictness both engines were given in the same change.
const FRAGMENTS: &[&str] = &[
    // The `at` shape: the reduce context takes a location, not a node.
    "there are no nodes to report at",
    // The position the world requires.
    "emit them on the fact during the per-file pass",
    // A fix cannot be carried: no parse tree, no node to attach one to.
    "there is no node to attach one to",
    // The message shape, for the strict reduce read.
    "takes a message: either a string, or { message }",
    // The per-file ambiguity: two fixes, neither one the host may pick.
    "as its third argument, not both",
];

#[test]
fn both_engines_refuse_a_broken_report_in_the_same_words() {
    for fragment in FRAGMENTS {
        assert!(
            HOST_RS.contains(fragment),
            "`crates/lanekeep-js/src/host.rs` no longer says `{fragment}`. Either the refusal \
             moved in the engine — in which case `host.js` has to move with it — or this canary \
             has outlived its wording, in which case both sides change together."
        );
        assert!(
            HOST_JS.contains(fragment),
            "`packages/lanekeep/runtime/host.js` no longer says `{fragment}`, which \
             `crates/lanekeep-js/src/host.rs` still does. The two engines' refusals are one \
             refusal; one has drifted, and a rule written against the other now reads prose \
             that does not match its engine."
        );
    }
}
