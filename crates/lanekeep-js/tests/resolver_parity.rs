//! The bundler's resolver refuses in the same words this crate's does.
//!
//! `RuleRoot::resolve` decides what a rule's imports mean at run time, inside the sandbox.
//! `packages/lanekeep/runtime/resolve.js` decides the same thing at build time, for a rule
//! that is bundled and compiled into a component before anything runs. The rules are enforced
//! **twice, in two languages, deliberately** — there is no third place to put them, because
//! one enforcement happens inside QuickJS and the other happens in a bundler that this
//! repository does not host.
//!
//! What that costs is a duplicate that nothing keeps honest. This is what keeps it honest, on
//! the model of `host_types.rs`, which holds `packages/lanekeep/index.d.ts` against `host.rs`
//! by reading both as text.
//!
//! **The messages are a proxy, and that is the point rather than a weakness.** Nobody is hurt
//! much by two refusals that differ in wording. What the check is really for is that the two
//! *rules* have drifted: a message is edited when the behavior behind it is edited, so prose
//! that no longer matches is the cheapest available signal that one side has moved and the
//! other has not. It is a canary, and a canary is chosen for being easy to watch.
//!
//! It only reads one direction — every Rust variant appears in the JavaScript. The other
//! direction is covered coarsely, by counting refusal codes, because parsing arbitrary
//! JavaScript for message literals would be a third thing to keep right.

use lanekeep_js::ResolveError;

/// The bundler's resolver, read at compile time so this cannot be pointed at a stale copy.
const RESOLVER: &str = include_str!("../../../packages/lanekeep/runtime/resolve.js");

/// Stands in for a variant's field, so the prose around it can be split out.
///
/// Both halves of a message's holes are interpolated differently on each side — Rust writes
/// `{specifier}` and JavaScript writes `${specifier}` — so the holes themselves are not
/// comparable and the prose between them is what gets asserted.
const HOLES: &[&str] = &[
    "<%specifier%>",
    "<%tried%>",
    "<%path%>",
    "<%detail%>",
    "<%name%>",
];

/// The literal prose of a message, with the holes taken out.
fn prose(rendered: &str) -> Vec<String> {
    let mut marked = rendered.to_owned();
    for hole in HOLES {
        marked = marked.replace(hole, "\u{0}");
    }
    marked
        .split('\u{0}')
        .filter(|fragment| !fragment.is_empty())
        .map(escape)
        .collect()
}

/// A fragment as it has to appear inside a JavaScript template literal.
///
/// Backslashes first: the two replacements after it each introduce one, and escaping those
/// again would be looking for a fragment nobody would ever write.
fn escape(fragment: &str) -> String {
    fragment
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('\n', "\\n")
}

/// Every variant, built with holes where its fields go.
fn every_refusal() -> Vec<(&'static str, ResolveError)> {
    vec![
        (
            "BareSpecifier",
            ResolveError::BareSpecifier {
                specifier: "<%specifier%>".to_owned(),
            },
        ),
        (
            "EscapesRoot",
            ResolveError::EscapesRoot {
                specifier: "<%specifier%>".to_owned(),
            },
        ),
        (
            "NotFound",
            ResolveError::NotFound {
                specifier: "<%specifier%>".to_owned(),
                tried: "<%tried%>".to_owned(),
            },
        ),
        (
            "Unreadable",
            ResolveError::Unreadable {
                path: "<%path%>".to_owned(),
                detail: "<%detail%>".to_owned(),
            },
        ),
        (
            "NotAModule",
            ResolveError::NotAModule {
                name: "<%name%>".to_owned(),
            },
        ),
    ]
}

#[test]
fn every_refusal_reads_the_same_from_a_bundler() {
    let mut asserted = 0_usize;
    let mut fragments = 0_usize;

    for (variant, error) in every_refusal() {
        for fragment in prose(&error.to_string()) {
            fragments += 1;
            asserted += fragment.len();
            assert!(
                RESOLVER.contains(&fragment),
                "`ResolveError::{variant}` says something the bundler's resolver does not. \
                 packages/lanekeep/runtime/resolve.js has no literal containing:\n  \
                 {fragment}\n  \
                 The two resolvers enforce one set of rules and a message that has drifted is \
                 the cheapest sign that the rules have. Reword both, or say in the pull \
                 request why one of them should read differently.\n  \
                 If the wording is unchanged, the message was probably split over a `+`: this \
                 looks for one unbroken template literal."
            );
        }
    }

    // A parse that quietly matched nothing would leave this green forever while checking
    // nothing. Five variants with one hole each contribute two fragments apiece, and the
    // shortest message is well over a hundred characters.
    assert!(
        fragments >= 10 && asserted >= 400,
        "only {fragments} fragments, {asserted} characters in all, were extracted — the \
         split is broken, so this test is asserting nothing"
    );
}

#[test]
fn the_bundler_refuses_for_no_reason_this_crate_lacks() {
    // The other direction, coarsely. A sixth refusal invented on the JavaScript side would be
    // a rule the sandbox does not enforce, which is the more dangerous asymmetry of the two —
    // and it would pass the test above, which only walks the variants this crate declares.
    //
    // Codes rather than messages, because reading them exactly would mean parsing JavaScript.
    let declared = every_refusal().len();
    let carried = RESOLVER.matches("code: '").count();

    assert_eq!(
        carried, declared,
        "packages/lanekeep/runtime/resolve.js carries {carried} refusal codes and \
         ResolveError has {declared} variants. A refusal on one side only is a rule enforced \
         at build time and not at run time, or the reverse."
    );
}

#[test]
fn the_table_above_cannot_fall_behind_the_enum() {
    // The match is the assertion: it has no wildcard, so a variant added to `ResolveError`
    // fails to compile here rather than being quietly left out of `every_refusal` — which
    // would leave a refusal the bundler never learned about, with both tests still green.
    for (_, error) in every_refusal() {
        match error {
            ResolveError::BareSpecifier { .. }
            | ResolveError::EscapesRoot { .. }
            | ResolveError::NotFound { .. }
            | ResolveError::Unreadable { .. }
            | ResolveError::NotAModule { .. } => {}
        }
    }
}
