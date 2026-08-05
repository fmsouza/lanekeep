//! A guest that runs scoped queries against a tree the host parsed and reports what it saw.
//!
//! **It is a probe, not a rule and not an assertion**, on exactly the terms
//! `tests/fixtures/navigation/src/lib.rs` sets out: every observation is encoded into the
//! message of a `report`, and the host asserts on the recorded reports. A guest that asserted
//! for itself could only fail by trapping, and a trap says nothing about which observation was
//! wrong.
//!
//! The probe to run is selected by the name of the first capture in the `match` handed to
//! `check`, so one component covers every case and each host-side test drives exactly one.
//!
//! # An error is reported, never unwrapped
//!
//! `query-subtree` and `closest-ancestor` return `result<_, string>`, and the whole reason
//! that channel exists is that a rule can *handle* a query it could not compile. A probe that
//! unwrapped would turn every such case into a trap and prove the opposite of the point, so
//! every call below matches and reports what it got — including the successes, where an
//! unexpected error is reported rather than swallowed.
//!
//! # Nothing here may index a slice
//!
//! A panic in a guest is a trap, and a trap aborts the call before any report crosses back —
//! so a shape assumption that turns out to be wrong would be reported as "the host recorded no
//! violations", which is also what a broken `report` looks like. Every lookup is a `get` or a
//! slice pattern, with an explicit `shape` report when the tree is not what this file expects.

#[allow(warnings)]
mod bindings;

use bindings::{CheckContext, Guest, Match, ReduceContext};

/// A handle no arena in these tests will have issued.
///
/// Rule code is arbitrary and may pass any number at all; what it must not do is take the
/// engine down with it.
const UNRESOLVABLE: u32 = 9999;

/// A query that does not compile: an unclosed group, which is a syntax error.
const MALFORMED: &str = "(((";

struct Component;

impl Guest for Component {
    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        false
    }

    fn check(ctx: &CheckContext, m: Match) {
        match m.first().map_or("", |entry| entry.name.as_str()) {
            "inside" => inside(ctx),
            "no-matches" => no_matches(ctx),
            "captures" => captures(ctx),
            "invalid" => invalid(ctx),
            "nearest" => nearest(ctx),
            "nothing" => nothing(ctx),
            "from-inside" => from_inside(ctx),
            "cache" => cache(ctx),
            "unresolvable" => unresolvable(ctx),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
    }

    /// A check-only rule still exports `reduce`, because a WIT world has no optional exports.
    fn reduce(_ctx: &ReduceContext) {}
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// The node a match bound to one capture name, if the pattern bound one.
///
/// By name rather than by position: a capture list is a `list<match-entry>` because the names
/// are the rule's own and not known to the world, so a probe that read `captures[0]` would be
/// asserting tree-sitter's reporting order rather than anything about the boundary.
fn capture(m: &Match, name: &str) -> Option<u32> {
    m.iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.node)
}

/// The first identifier bound by `@n`, which is where the ancestor probes start walking from.
fn first_identifier(ctx: &CheckContext) -> Option<u32> {
    ctx.query_subtree(ctx.root(), "(variable_declarator name: (identifier) @n)")
        .ok()?
        .first()
        .and_then(|m| capture(m, "n"))
}

/// A query scoped to one node finds what is inside that node and nothing else.
fn inside(ctx: &CheckContext) {
    let functions = match ctx.query_subtree(ctx.root(), "(function_declaration) @fn") {
        Ok(found) => found,
        Err(problem) => return say(ctx, &format!("unexpected error: {problem}")),
    };

    let Some(second) = functions.get(1).and_then(|m| capture(m, "fn")) else {
        return say(
            ctx,
            &format!("shape: expected two functions, found {}", functions.len()),
        );
    };

    let inner = match ctx.query_subtree(second, "(variable_declarator name: (identifier) @name)") {
        Ok(found) => found,
        Err(problem) => return say(ctx, &format!("unexpected error: {problem}")),
    };

    let names: Vec<String> = inner
        .iter()
        .filter_map(|m| capture(m, "name"))
        .filter_map(|node| ctx.text(node))
        .collect();

    // The whole-file count as well as the scoped one. Without it a scoped result that
    // happened to be complete would be indistinguishable from a scope that was ignored.
    let everywhere = ctx
        .query_subtree(ctx.root(), "(variable_declarator name: (identifier) @name)")
        .map_or(0, |found| found.len());

    say(
        ctx,
        &format!(
            "functions={} scoped={} whole-file={}",
            functions.len(),
            names.join(","),
            everywhere
        ),
    );
}

/// A query that matches nothing is an empty list, not an error and not an absence.
fn no_matches(ctx: &CheckContext) {
    let rendered = match ctx.query_subtree(ctx.root(), "(debugger_statement) @d") {
        Ok(found) => format!("ok len={}", found.len()),
        Err(problem) => format!("error: {problem}"),
    };
    say(ctx, &rendered);
}

/// Every capture crosses under the name the query gave it, bound to a usable handle.
fn captures(ctx: &CheckContext) {
    let found = match ctx.query_subtree(
        ctx.root(),
        "(variable_declarator name: (identifier) @name value: (number) @value)",
    ) {
        Ok(found) => found,
        Err(problem) => return say(ctx, &format!("unexpected error: {problem}")),
    };

    let Some(m) = found.first() else {
        return say(ctx, "shape: expected one declarator");
    };

    // Sorted, so the assertion is about the names and the nodes rather than about the order
    // tree-sitter happened to report them in.
    let mut rendered: Vec<String> = m
        .iter()
        .map(|entry| {
            format!(
                "{}={}",
                entry.name,
                ctx.text(entry.node).unwrap_or_else(|| "<none>".to_owned())
            )
        })
        .collect();
    rendered.sort();

    let Some(value) = capture(m, "value") else {
        return say(ctx, "shape: the pattern bound no `value`");
    };

    // Reported *at* the captured node, so the host sees the position it derived itself beside
    // the one the guest read through `line` and `column`.
    ctx.report(
        value,
        Some(&format!(
            "{} line={:?} column={:?}",
            rendered.join(" "),
            ctx.line(value),
            ctx.column(value)
        )),
        None,
    );
}

/// A query that does not compile reaches the rule as an error it can act on.
fn invalid(ctx: &CheckContext) {
    let subtree = match ctx.query_subtree(ctx.root(), MALFORMED) {
        Ok(found) => format!("ok with {} matches", found.len()),
        Err(problem) => problem,
    };
    say(ctx, &format!("query-subtree: {subtree}"));

    // The same source through the other method, because both compile through one cache and a
    // failure recorded by one is handed to the other.
    let ancestor = match ctx.closest_ancestor(ctx.root(), MALFORMED) {
        Ok(found) => format!("ok with {}", if found.is_some() { "some" } else { "none" }),
        Err(problem) => problem,
    };
    say(ctx, &format!("closest-ancestor: {ancestor}"));
}

/// The nearest matching ancestor, not the outermost.
fn nearest(ctx: &CheckContext) {
    let Some(start) = first_identifier(ctx) else {
        return say(ctx, "shape: no declarator to start from");
    };

    let found = match ctx.closest_ancestor(
        start,
        "(function_declaration name: (identifier) @name) @fn",
    ) {
        Ok(found) => found,
        Err(problem) => return say(ctx, &format!("unexpected error: {problem}")),
    };

    let Some(m) = found else {
        return say(ctx, "nothing matched");
    };
    let Some(name) = capture(&m, "name") else {
        return say(ctx, "shape: the match bound no `name`");
    };

    say(
        ctx,
        &format!(
            "nearest={} captures={}",
            ctx.text(name).unwrap_or_else(|| "<none>".to_owned()),
            m.len()
        ),
    );
}

/// Nothing matched is `none`, which is a different value from a match with no captures.
fn nothing(ctx: &CheckContext) {
    let Some(start) = first_identifier(ctx) else {
        return say(ctx, "shape: no declarator to start from");
    };

    let rendered = match ctx.closest_ancestor(start, "(class_declaration) @c") {
        Ok(None) => "none".to_owned(),
        Ok(Some(m)) => format!("some with {} captures", m.len()),
        Err(problem) => format!("error: {problem}"),
    };
    say(ctx, &rendered);
}

/// A query matching something *inside* an ancestor must not make that ancestor the answer.
fn from_inside(ctx: &CheckContext) {
    let Some(start) = first_identifier(ctx) else {
        return say(ctx, "shape: no declarator to start from");
    };

    // Every ancestor of `x` contains a number, and none of them *is* one. An implementation
    // that asked "does the query match anywhere below this ancestor" would answer with the
    // very first one and never look further, which makes the outermost node the answer to
    // every upward walk.
    let within = match ctx.closest_ancestor(start, "(number) @num") {
        Ok(None) => "none".to_owned(),
        Ok(Some(m)) => format!(
            "matched {}",
            capture(&m, "num")
                .and_then(|node| ctx.kind(node))
                .unwrap_or_else(|| "<none>".to_owned())
        ),
        Err(problem) => format!("error: {problem}"),
    };

    // Paired with one that does match, from the same node in the same tree: a host that
    // answered `none` to everything would pass the line above on its own.
    let at = match ctx.closest_ancestor(start, "(statement_block) @block") {
        Ok(None) => "none".to_owned(),
        Ok(Some(m)) => capture(&m, "block")
            .and_then(|node| ctx.text(node))
            .unwrap_or_else(|| "<none>".to_owned()),
        Err(problem) => format!("error: {problem}"),
    };

    say(ctx, &format!("within={within} at={at}"));
}

/// The same query source, over and over, through both methods.
///
/// The performance property `docs/architecture.md` §6.2 states: a rule calling `querySubtree`
/// inside its handler calls it once per match with the same string every time, so compiling
/// per call would make the second-cheapest operation in the host the most expensive one. The
/// host counts compilations and its test asserts the count; this only has to do the calling.
fn cache(ctx: &CheckContext) {
    let good = "(lexical_declaration) @d";
    for _ in 0..3 {
        if let Err(problem) = ctx.query_subtree(ctx.root(), good) {
            return say(ctx, &format!("unexpected error: {problem}"));
        }
    }

    // Through the other method, which shares the cache: keyed by source, not by caller.
    if let Err(problem) = ctx.closest_ancestor(ctx.root(), good) {
        return say(ctx, &format!("unexpected error: {problem}"));
    }

    // And a failure twice, because a cache that only remembers successes recompiles a bad
    // query on every match — the one case where the work is wasted for certain.
    for _ in 0..2 {
        if ctx.query_subtree(ctx.root(), MALFORMED).is_ok() {
            return say(ctx, "the malformed query compiled");
        }
    }

    say(ctx, "six calls over two sources");
}

/// A handle that resolves to nothing, through both query methods.
fn unresolvable(ctx: &CheckContext) {
    let subtree = match ctx.query_subtree(UNRESOLVABLE, "(lexical_declaration) @d") {
        Ok(found) => format!("ok len={}", found.len()),
        Err(problem) => format!("error: {problem}"),
    };
    let ancestor = match ctx.closest_ancestor(UNRESOLVABLE, "(program) @p") {
        Ok(None) => "ok none".to_owned(),
        Ok(Some(m)) => format!("ok some with {} captures", m.len()),
        Err(problem) => format!("error: {problem}"),
    };
    say(ctx, &format!("subtree={subtree} ancestor={ancestor}"));
}

bindings::export!(Component with_types_in bindings);
