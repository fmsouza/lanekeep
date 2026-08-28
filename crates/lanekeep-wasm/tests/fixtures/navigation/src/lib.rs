//! A guest that walks a tree the host parsed and reports what it saw.
//!
//! **It is a probe, not a rule and not an assertion.** Every observation it makes is encoded
//! into the message of a `report`, and the host asserts on the recorded reports — both on the
//! message the guest composed and on the line and column the *host* derived from the node
//! reported at. Putting the assertions on the host side is what makes a failure legible: a
//! guest that asserted for itself could only fail by trapping, and a trap says nothing about
//! which of nine observations was wrong.
//!
//! The probe to run is selected by the name of the first capture in the `match` handed to
//! `check`, so one component covers every case and each host-side test drives exactly one.
//! Anything unrecognized reports as much rather than doing nothing, because a probe that
//! silently does nothing and a host that recorded nothing look identical.
//!
//! # Nothing here may index a slice
//!
//! A panic in a guest is a trap, and a trap aborts the call before any report crosses back —
//! so a shape assumption that turns out to be wrong would be reported as "the host recorded
//! no violations", which is also what a broken `report` looks like. Every lookup is therefore
//! a slice pattern or a `get`, with an explicit `shape` report when the tree is not what this
//! file expects.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    QueryFor,
    Fix, RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// A handle no arena in these tests will have issued.
///
/// Rule code is arbitrary and may pass any number at all; what it must not do is take the
/// engine down with it.
const UNRESOLVABLE: u32 = 9999;

struct Component;

impl Guest for Component {
    /// The one rule this component hosts. Every other export takes its index.
    fn rules() -> Vec<String> {
        vec!["fixture/navigation".to_owned()]
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata(rule: u32) -> RuleMetadata {
        only(rule);
        RuleMetadata {
            id: "fixture/navigation".to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: String::new(),
                remediation: String::new(),
                examples: RuleExamples {
                    bad: String::new(),
                    good: String::new(),
                },
            },

            queries: vec![QueryFor { language: "rust".to_owned(), query: String::new() }],
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `configure` itself is tested.
    ///
    /// Refuses unconditionally rather than accepting anything, so a caller that reached this
    /// export on this fixture fails loudly instead of passing on a vacuous success.
    fn configure(rule: u32, _options_json: String) -> Result<(), String> {
        only(rule);
        Err("fixture/navigation does not implement configure".to_owned())
    }

    fn has_check(rule: u32) -> bool {
        only(rule);
        true
    }

    fn has_reduce(rule: u32) -> bool {
        only(rule);
        false
    }

    fn check(rule: u32, ctx: &CheckContext, m: Match) -> Result<(), RuleError> {
        only(rule);
        match m.first().map_or("", |entry| entry.name.as_str()) {
            "file" => file(ctx),
            "navigate" => navigate(ctx),
            "walk" => walk(ctx),
            "ancestors" => ancestors(ctx),
            "named" => named(ctx),
            "unresolvable" => unresolvable(ctx),
            "loc" => loc(ctx),
            "fix" => fix(ctx),
            "unsaid-fix" => unsaid_fix(ctx),
            "bare" => bare(ctx),
            "today" => today(ctx),
            "fingerprint" => fingerprint(ctx),
            "trap-after-report" => trap_after_report(ctx),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
        Ok(())
    }

    /// A check-only rule still exports `reduce`, because a WIT world has no optional exports.
    fn reduce(rule: u32, _ctx: &ReduceContext) -> Result<(), RuleError> {
        only(rule);
        Ok(())
    }
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// The two whole-file accessors.
fn file(ctx: &CheckContext) {
    say(ctx, &format!("{} | {}", ctx.file_path(), ctx.file_text()));
}

/// Descending from the root, reading kinds and text and positions on the way.
fn navigate(ctx: &CheckContext) {
    let root = ctx.root();
    let kind = ctx.kind(root);
    ctx.report(root, kind.as_deref(), None);

    let children = ctx.named_children(root);
    say(ctx, &format!("{} named children", children.len()));

    let [first, second, ..] = children.as_slice() else {
        return say(
            ctx,
            "shape: expected at least two named children of the root",
        );
    };

    let kind = ctx.kind(*first);
    ctx.report(*first, kind.as_deref(), None);

    let text = ctx.text(*second);
    ctx.report(*second, text.as_deref(), None);

    // `line` and `column` read through their own methods rather than through the position
    // the host attaches to a report, because they are separate methods and could disagree.
    ctx.report(
        *second,
        Some(&format!(
            "line={:?} column={:?}",
            ctx.line(*second),
            ctx.column(*second)
        )),
        None,
    );
}

/// Walking down and back up, and the identity two routes to one node have to share.
fn walk(ctx: &CheckContext) {
    let root = ctx.root();
    let Some(&declaration) = ctx.named_children(root).first() else {
        return say(ctx, "shape: the root has no named children");
    };
    let Some(&inner) = ctx.named_children(declaration).first() else {
        return say(ctx, "shape: the declaration has no named children");
    };

    ctx.report(
        root,
        Some(&format!(
            "parent-of-inner-is-declaration={} parent-of-declaration-is-root={} \
             root-has-parent={} same-node-same-handle={}",
            ctx.parent(inner) == Some(declaration),
            ctx.parent(declaration) == Some(root),
            ctx.parent(root).is_some(),
            ctx.named_children(root).first() == Some(&declaration),
        )),
        None,
    );

    // Stated in raw numbers as well as in relations, because the failure this guards against
    // has a shape a boolean hides: the root's handle is zero, so a host that treated absence
    // and zero as the same thing would report `parent-of-declaration-is-root=false` above and
    // `parent-of-declaration=None` here, and only the second says which of the two it did.
    ctx.report(
        root,
        Some(&format!(
            "root={root} parent-of-declaration={:?}",
            ctx.parent(declaration)
        )),
        None,
    );
}

/// Walking all the way up, from a statement nested two levels down.
fn ancestors(ctx: &CheckContext) {
    let root = ctx.root();
    let Some(&function) = ctx.named_children(root).first() else {
        return say(ctx, "shape: the root has no named children");
    };
    let Some(&body) = ctx.named_children(function).last() else {
        return say(ctx, "shape: the function has no named children");
    };
    let Some(&statement) = ctx.named_children(body).first() else {
        return say(ctx, "shape: the body has no named children");
    };

    let chain = ctx.ancestors(statement);
    ctx.report(
        statement,
        Some(&format!(
            "len={} first-is-body={} last-is-root={} contains-function={} root-ancestors={}",
            chain.len(),
            chain.first() == Some(&body),
            chain.last() == Some(&root),
            chain.contains(&function),
            ctx.ancestors(root).len(),
        )),
        None,
    );
}

/// Named children against all children, and what `is-named` says about each.
fn named(ctx: &CheckContext) {
    let Some(&declaration) = ctx.named_children(ctx.root()).first() else {
        return say(ctx, "shape: the root has no named children");
    };

    let all = ctx.children(declaration);
    let named = ctx.named_children(declaration);
    ctx.report(
        declaration,
        Some(&format!(
            "all={} named={} first-token-is-named={} every-named-child-is-named={}",
            all.len(),
            named.len(),
            all.first().is_some_and(|&handle| ctx.is_named(handle)),
            named.iter().all(|&handle| ctx.is_named(handle)),
        )),
        None,
    );
}

/// A handle that resolves to nothing, through every method that takes one.
fn unresolvable(ctx: &CheckContext) {
    say(
        ctx,
        &format!(
            "kind={:?} text={:?} line={:?} column={:?} parent={:?} is-named={} \
             children={} named-children={} ancestors={} loc={}",
            ctx.kind(UNRESOLVABLE),
            ctx.text(UNRESOLVABLE),
            ctx.line(UNRESOLVABLE),
            ctx.column(UNRESOLVABLE),
            ctx.parent(UNRESOLVABLE),
            ctx.is_named(UNRESOLVABLE),
            ctx.children(UNRESOLVABLE).len(),
            ctx.named_children(UNRESOLVABLE).len(),
            ctx.ancestors(UNRESOLVABLE).len(),
            ctx.loc(UNRESOLVABLE).is_some(),
        ),
    );

    // And reporting *at* one, which must leave nothing behind rather than land at a made-up
    // position a reader would go and look at.
    ctx.report(UNRESOLVABLE, Some("this must not be recorded"), None);
}

/// The position a rule reads when it wants to reason about a node rather than report at it.
fn loc(ctx: &CheckContext) {
    let children = ctx.named_children(ctx.root());
    let Some(&second) = children.get(1) else {
        return say(ctx, "shape: the root has fewer than two named children");
    };

    let rendered = ctx.loc(second).map_or_else(
        || "none".to_owned(),
        |at| format!("{}:{}:{}", at.file, at.line, at.column),
    );
    ctx.report(second, Some(&rendered), None);
}

/// Reporting with a fix: safe, unsafe, and one naming a node that does not resolve.
fn fix(ctx: &CheckContext) {
    let children = ctx.named_children(ctx.root());
    let [first, second, ..] = children.as_slice() else {
        return say(ctx, "shape: the root has fewer than two named children");
    };

    ctx.report(
        *second,
        Some("replace it"),
        Some(&Fix {
            node: *second,
            text: "const y = 3;".to_owned(),
            safe: Some(true),
        }),
    );

    ctx.report(
        *first,
        Some("a suggestion"),
        Some(&Fix {
            node: *first,
            text: "const x = 2;".to_owned(),
            safe: Some(false),
        }),
    );

    // The fix names a node the arena cannot resolve. Dropping the fix and keeping the report
    // is the only sound answer: a fix at guessed offsets would rewrite the wrong code.
    ctx.report(
        *first,
        Some("a fix at a handle that does not resolve"),
        Some(&Fix {
            node: UNRESOLVABLE,
            text: "nope".to_owned(),
            safe: Some(true),
        }),
    );
}

/// A fix that does not say whether it is safe.
///
/// `safe` is `option<bool>` precisely so this is expressible: a rule that did not decide has
/// said something different from a rule that decided "yes", and the host is what turns the
/// first into a suggestion. A `bool` would have made this guest pick a value, which is the
/// choice that must not be the guest's.
fn unsaid_fix(ctx: &CheckContext) {
    let Some(&first) = ctx.named_children(ctx.root()).first() else {
        return say(ctx, "shape: the root has no named children");
    };

    ctx.report(
        first,
        Some("did not say"),
        Some(&Fix {
            node: first,
            text: "const x = 3;".to_owned(),
            safe: None,
        }),
    );
}

/// `report`'s no-message form: both optional parameters absent, independently.
fn bare(ctx: &CheckContext) {
    ctx.report(ctx.root(), None, None);
}

/// The one thing about the environment a rule is permitted to observe.
///
/// Rendered rather than branched on, so the host asserts on the whole answer and on nothing
/// this guest decided. `None` is the world's declared meaning for "not permitted to observe it"
/// and not an error the guest should be smoothing over, which is why there is no `unwrap_or`
/// here — a probe that substituted a placeholder for `none` would make the two host
/// configurations indistinguishable in the report the host asserts on.
///
/// Called exactly once. What the host checks alongside the answer is that asking was
/// *recorded*, and a probe that asked twice could not tell a flag that latches from one that
/// counts.
fn today(ctx: &CheckContext) {
    say(ctx, &format!("today={:?}", ctx.today()));
}

/// The structural-summary method, on the real host.
///
/// The hash and count are the host's own fold; this probe reports them so the host-side test
/// can compare them against a fresh arena's answer. Also probes the `none` branch — an
/// unresolvable handle must come back as `None` rather than as a fabricated shape.
fn fingerprint(ctx: &CheckContext) {
    let live = ctx.structure_fingerprint(ctx.root());
    match live {
        Some(fp) => say(ctx, &format!("fp={}:{}", fp.hash, fp.nodes)),
        None => say(ctx, "fp=none-for-root"),
    }
    let dead = ctx.structure_fingerprint(UNRESOLVABLE);
    say(ctx, &format!("dead={:?}", dead.is_none()));
}

/// A report, and then a call this host has no answer to give.
///
/// The property is about *reporting*: a handler may report several times and then hit something
/// it cannot do, and what it already found is still true. The host keeps those reports rather
/// than discarding them with the instance.
///
/// The trap it uses is incidental and had to change. This probe used to call `today`, which
/// trapped while it was declared and unimplemented; now that every `check-context` method is
/// implemented, the only refusal left on this surface is `read-file` on a context built without
/// file access — which `tests/navigation.rs` builds, and which `src/host.rs`'s module header
/// explains. The host asserts on the trap's root cause, so a *different* trap arriving here
/// fails the test rather than passing it.
fn trap_after_report(ctx: &CheckContext) {
    say(ctx, "recorded before the trap");
    let refused = ctx.read_file("anything.json");
    say(ctx, &format!("unreachable, and yet: {refused:?}"));
}

/// The one rule this component hosts.
///
/// A component hosts a *list* of rules and every export but `rules` takes an index into it.
/// This one hosts a single rule, so zero is the only index that answers — and a host asking
/// for another has disagreed with what `rules` reported, which is worth trapping on rather
/// than answering with the one rule's data under another rule's name.
fn only(rule: u32) {
    assert_eq!(rule, 0, "this component hosts one rule");
}

bindings::export!(Component with_types_in bindings);
