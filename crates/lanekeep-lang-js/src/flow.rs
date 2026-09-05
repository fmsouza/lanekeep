//! The taint (data-flow) analysis for TypeScript, TSX and JavaScript.
//!
//! A value-level may-taint: a `@source` value that reaches a `@sink` with no intervening
//! `@sanitizer` is one [`FlowPath`]. It is resolved on demand — from each sink backward
//! through the def-use chain (declarators and reassignments), following local identifier
//! aliases and cutting at sanitizers — with the per-function [`Cfg`](crate::cfg::Cfg)'s
//! reachability deciding whether a definition reaches the sink (the flow-sensitivity).
//!
//! v1 is intra-procedural, path-insensitive, and does not follow taint through a call's
//! arguments. See
//! `docs/superpowers/specs/2026-09-05-taint-analysis-flow-checkflow-design.md` §5.

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::flow::{FlowAnalyzer, FlowPath};
use tree_sitter::{Node, Tree};

use crate::binding::JsBindingResolver;
use crate::cfg::{BlockId, Cfg};

/// The depth ceiling on the alias walk, copied from the type oracle
/// (`crates/lanekeep-types/src/oracle.rs`). Bounds a cyclic `const a = b; const b = a`.
const MAX_DEPTH: u32 = 16;

/// The JS/TS taint analyzer, returned from [`crate::TypeScript::flow_analyzer`] and its
/// siblings.
pub(crate) struct JsFlowAnalyzer;

impl FlowAnalyzer for JsFlowAnalyzer {
    fn analyze<'t>(
        &self,
        tree: &'t Tree,
        source: &str,
        sources: &[Node<'t>],
        sinks: &[Node<'t>],
        sanitizers: &[Node<'t>],
    ) -> Vec<FlowPath<'t>> {
        let mut flows = Vec::new();
        for &sink in sinks {
            // Each sink is analyzed in its own enclosing function's CFG. Rebuilding per
            // sink keeps the borrow simple; the fixtures hold one or two functions.
            let root_and_cfg = enclosing_root_and_cfg(source, sink);
            let (root, cfg) = match &root_and_cfg {
                Some((root, cfg)) => (Some(*root), Some(cfg)),
                None => (None, None),
            };
            let sink_block = cfg.and_then(|cfg| cfg.block_of(sink));
            let taint = Taint {
                tree,
                source,
                sources,
                sanitizers,
                cfg,
                root,
            };
            for fact in taint.taint_of(sink, sink_block, 0) {
                flows.push(FlowPath {
                    source: fact.source,
                    sink,
                    steps: fact.steps,
                });
            }
        }
        canonicalize(flows)
    }
}

/// One reason a value is tainted: the originating source, and the alias hops between it and
/// the value, in flow order (source → value).
struct Fact<'t> {
    source: Node<'t>,
    steps: Vec<Node<'t>>,
}

/// A definition of a binding: the assignment site (declarator or `=` expression) and the
/// right-hand-side value it stores.
struct Def<'t> {
    /// The `variable_declarator` or `assignment_expression` — the step recorded for an alias
    /// hop.
    site: Node<'t>,
    /// The value expression assigned.
    rhs: Node<'t>,
}

/// The immutable context for one sink's taint walk.
struct Taint<'a, 't> {
    tree: &'t Tree,
    source: &'a str,
    sources: &'a [Node<'t>],
    sanitizers: &'a [Node<'t>],
    /// The enclosing function's CFG, or `None` when the sink owns no flow graph. A missing
    /// graph makes reachability unanswerable, so the walk then admits every definition —
    /// the may-analysis's correct over-approximating bias.
    cfg: Option<&'a Cfg<'t>>,
    /// The enclosing function's root node, whose subtree is scanned for reassignments.
    root: Option<Node<'t>>,
}

impl<'t> Taint<'_, 't> {
    /// The taint facts an expression carries when read at the sink.
    ///
    /// Value-level: a `@sanitizer` call yields a clean value regardless of its arguments,
    /// an arbitrary non-source call carries nothing (v1 does not track taint through a
    /// call), and only a direct `@source` or a local alias of a tainted binding is tainted.
    fn taint_of(&self, expr: Node<'t>, sink_block: Option<BlockId>, depth: u32) -> Vec<Fact<'t>> {
        if depth >= MAX_DEPTH {
            return Vec::new();
        }
        // A sanitizer call's result is clean — cut the value — regardless of its arguments,
        // and before any source it textually wraps is considered. This is what makes
        // `redact(getSecret())` silent where `foo(getSecret())` is not.
        if is_member(expr, self.sanitizers) {
            return Vec::new();
        }
        // A `@source` appearing within the expression taints it: identity when the sink or
        // value *is* the source call, containment when it wraps one (`getSecret() + x`).
        // Taint carried by a *binding* through a call is a different question, answered
        // below by def-use — and `identity(a)` wraps no source, so it stays clean (the v1
        // alias-through-call false negative).
        let direct = self.sources_within(expr);
        if !direct.is_empty() {
            return direct
                .into_iter()
                .map(|source| Fact {
                    source,
                    steps: Vec::new(),
                })
                .collect();
        }
        match expr.kind() {
            "identifier" => self.taint_of_identifier(expr, sink_block, depth),
            // A non-source, non-sanitizer call is opaque: v1 does not follow taint through a
            // call's arguments (the alias-through-call false negative, spec §13). Only a
            // direct source or a local identifier alias carries taint.
            _ => Vec::new(),
        }
    }

    /// The taint facts an identifier read carries: resolve it to its declaration and follow
    /// each reaching definition.
    fn taint_of_identifier(
        &self,
        ident: Node<'t>,
        sink_block: Option<BlockId>,
        depth: u32,
    ) -> Vec<Fact<'t>> {
        let Some(decl) = JsBindingResolver.declaration_of(self.tree, self.source, ident) else {
            return Vec::new();
        };
        let mut facts = Vec::new();
        for def in self.definitions_of(decl) {
            if !self.reaches_sink(def.rhs, sink_block) {
                continue;
            }
            let alias = def.rhs.kind() == "identifier";
            for mut fact in self.taint_of(def.rhs, sink_block, depth.saturating_add(1)) {
                if alias {
                    // A `const b = a` / `b = a` hop is one step; a direct source assignment
                    // adds none.
                    fact.steps.push(def.site);
                }
                facts.push(fact);
            }
        }
        facts
    }

    /// Every definition of the binding `decl` declares: its declarator's own initializer,
    /// plus every `x = <rhs>` in the enclosing function whose target resolves back to `decl`.
    /// Path-insensitive — a binding assigned in two branches has two definitions, and both
    /// are followed.
    fn definitions_of(&self, decl: Node<'t>) -> Vec<Def<'t>> {
        let mut defs = Vec::new();
        if decl.kind() == "variable_declarator"
            && let Some(value) = decl.child_by_field_name("value")
        {
            defs.push(Def {
                site: decl,
                rhs: value,
            });
        }
        for assignment in self.assignments_to(decl) {
            if let Some(rhs) = assignment.child_by_field_name("right") {
                defs.push(Def {
                    site: assignment,
                    rhs,
                });
            }
        }
        defs
    }

    /// Every `assignment_expression` in the enclosing function whose left-hand identifier
    /// resolves to `decl`, in source order. Empty without a root to scan.
    fn assignments_to(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "assignment_expression"
                && let Some(left) = node.child_by_field_name("left")
                && left.kind() == "identifier"
                && JsBindingResolver
                    .declaration_of(self.tree, self.source, left)
                    .is_some_and(|target| target.id() == decl.id())
            {
                found.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        found.sort_by_key(Node::start_byte);
        found
    }

    /// The `@source` nodes lying within `expr`'s byte range, in source order. A source that
    /// *is* `expr` is included (its range covers itself), so this subsumes the direct case.
    fn sources_within(&self, expr: Node<'t>) -> Vec<Node<'t>> {
        let mut found: Vec<Node<'t>> = self
            .sources
            .iter()
            .copied()
            .filter(|source| {
                expr.start_byte() <= source.start_byte() && source.end_byte() <= expr.end_byte()
            })
            .collect();
        found.sort_by_key(Node::start_byte);
        found
    }

    /// Whether a definition's block can reach the sink's block. Absent a CFG, or a block for
    /// either node, the definition is admitted (over-approximate may-taint).
    fn reaches_sink(&self, def: Node<'t>, sink_block: Option<BlockId>) -> bool {
        let (Some(cfg), Some(sink_block)) = (self.cfg, sink_block) else {
            return true;
        };
        match cfg.block_of(def) {
            Some(def_block) => cfg.reaches(def_block, sink_block),
            None => true,
        }
    }
}

/// Whether `node` is one of `set`, by tree-unique node identity.
fn is_member(node: Node<'_>, set: &[Node<'_>]) -> bool {
    set.iter().any(|member| member.id() == node.id())
}

/// The nearest enclosing function's root node and its CFG, walking ancestors until
/// [`Cfg::build`] accepts one as a root kind. `program` is a root, so a match is always found
/// for a node in a parsed tree.
fn enclosing_root_and_cfg<'t>(source: &str, node: Node<'t>) -> Option<(Node<'t>, Cfg<'t>)> {
    let mut current = Some(node);
    while let Some(node) = current {
        if let Some(cfg) = Cfg::build(source, node) {
            return Some((node, cfg));
        }
        current = node.parent();
    }
    None
}

/// Reduce raw flows to one canonical [`FlowPath`] per `(source, sink)`: keep the shortest
/// `steps` chain (ties by the first differing step's start byte), drop exact duplicates, and
/// emit in `(sink start, source start)` order. Determinism rests on this sort and on nothing
/// here iterating a hash container.
fn canonicalize(mut flows: Vec<FlowPath<'_>>) -> Vec<FlowPath<'_>> {
    // Group identical `(source, sink)` pairs together with the shortest chain first, so the
    // dedup below keeps the shortest. Node ranges identify source and sink; `steps` positions
    // give a total order for the two-runs-identical guarantee.
    flows.sort_by(|a, b| {
        pair_key(a)
            .cmp(&pair_key(b))
            .then_with(|| a.steps.len().cmp(&b.steps.len()))
            .then_with(|| step_positions(a).cmp(&step_positions(b)))
    });
    flows.dedup_by(|a, b| pair_key(a) == pair_key(b));
    // Final output order: sink position, then source position (ranges break ties totally).
    flows.sort_by_key(|flow| {
        (
            flow.sink.start_byte(),
            flow.source.start_byte(),
            flow.sink.end_byte(),
            flow.source.end_byte(),
        )
    });
    flows
}

/// The `(source range, sink range)` identity of a flow — what makes two flows the same
/// `(source, sink)` pair.
fn pair_key(flow: &FlowPath<'_>) -> (usize, usize, usize, usize) {
    (
        flow.source.start_byte(),
        flow.source.end_byte(),
        flow.sink.start_byte(),
        flow.sink.end_byte(),
    )
}

/// A flow's step start bytes, for a deterministic tie-break between equal-length chains.
fn step_positions(flow: &FlowPath<'_>) -> Vec<usize> {
    flow.steps.iter().map(Node::start_byte).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::testing::{find_all, parse};

    /// The identifier a call's callee names, when it is a plain identifier.
    fn callee_name<'a>(call: Node<'_>, source: &'a str) -> Option<&'a str> {
        let callee = call.child_by_field_name("function")?;
        (callee.kind() == "identifier").then(|| &source[callee.byte_range()])
    }

    /// Every `call_expression` whose callee identifier is exactly `name`, source order.
    fn calls_named<'t>(tree: &'t Tree, source: &str, name: &str) -> Vec<Node<'t>> {
        find_all(tree, "call_expression")
            .into_iter()
            .filter(|call| callee_name(*call, source) == Some(name))
            .collect()
    }

    /// The first-argument expression of every call to `name`, source order.
    fn sink_args<'t>(tree: &'t Tree, source: &str, name: &str) -> Vec<Node<'t>> {
        calls_named(tree, source, name)
            .into_iter()
            .filter_map(|call| {
                call.child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0))
            })
            .collect()
    }

    /// Parse `src`, pick source/sink/sanitizer nodes by callee name, run the analyzer, and
    /// return `(source text, sink text)` for each reported flow. Stable across 4a–4e.
    fn run(
        src: &str,
        source_name: &str,
        sink_name: &str,
        sanitizer_name: &str,
    ) -> Vec<(String, String)> {
        let tree = parse(src);
        let sources = calls_named(&tree, src, source_name);
        let sinks = sink_args(&tree, src, sink_name);
        let sanitizers = calls_named(&tree, src, sanitizer_name);
        JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| {
                (
                    src[flow.source.byte_range()].to_owned(),
                    src[flow.sink.byte_range()].to_owned(),
                )
            })
            .collect()
    }

    /// Like [`run`], but also reporting each flow's `steps` length — for the canonical-path
    /// assertions where the number of hops matters.
    fn run_full(
        src: &str,
        source_name: &str,
        sink_name: &str,
        sanitizer_name: &str,
    ) -> Vec<(String, String, usize)> {
        let tree = parse(src);
        let sources = calls_named(&tree, src, source_name);
        let sinks = sink_args(&tree, src, sink_name);
        let sanitizers = calls_named(&tree, src, sanitizer_name);
        JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| {
                (
                    src[flow.source.byte_range()].to_owned(),
                    src[flow.sink.byte_range()].to_owned(),
                    flow.steps.len(),
                )
            })
            .collect()
    }

    #[test]
    fn a_source_used_directly_as_a_sink_argument_reports() {
        // log(getSecret()) — the sink argument *is* the source call.
        let flows = run(
            "function f() { log(getSecret()); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn taint_through_one_assignment_reports() {
        let flows = run(
            "function f() { const s = getSecret(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn taint_through_two_assignments_reports() {
        let flows = run(
            "function f() { const s = getSecret(); const t = s; log(t); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_clean_local_does_not_report() {
        // const s = clean(); log(s); — clean() is neither a source nor a sanitizer.
        let flows = run(
            "function f() { const s = clean(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    #[test]
    fn a_sanitizer_before_the_sink_cuts() {
        // reads the *clean* value c, not s → silent.
        let flows = run(
            "function f() { const s = getSecret(); const c = redact(s); log(c); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    #[test]
    fn a_sanitizer_after_the_sink_does_not_cut() {
        // log reads s while it is still tainted; redact runs afterward.
        let flows = run(
            "function f() { const s = getSecret(); log(s); const c = redact(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn a_sanitizer_wrapping_a_source_cuts() {
        // redact(getSecret()) textually wraps the source; the cut must win over containment,
        // or c is tainted. This is what makes the sanitizer check load-bearing: without it,
        // the wrapped source would taint c.
        let flows = run(
            "function f() { const c = redact(getSecret()); log(c); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    #[test]
    fn a_const_alias_propagates_taint() {
        let flows = run(
            "function f() { const a = getSecret(); const b = a; log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn aliasing_through_a_call_does_not_propagate() {
        // Documented v1 false NEGATIVE: identity(a) is not tracked.
        let flows = run(
            "function f() { const a = getSecret(); const b = identity(a); log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "v1 does not follow taint through a call");
    }

    #[test]
    fn two_sources_into_one_sink_dedup_deterministically() {
        // both branches taint s; path-insensitive → both reach log(s).
        let src = "function f(c) { let s; if (c) { s = getSecret(); } else { s = getSecret(); } log(s); }";
        let flows = run(src, "getSecret", "log", "redact");
        // One canonical flow per (source, sink); ordered by source position; no duplicates.
        assert_eq!(flows.len(), 2);
        assert!(flows[0].1 == flows[1].1, "same sink");
        // The two sources are the two distinct getSecret() calls, in source order.
        assert!(
            flows[0].0.starts_with("getSecret") && flows[1].0.starts_with("getSecret"),
            "both flows originate at a source"
        );
        // Determinism: running twice gives identical ordering.
        let again = run(src, "getSecret", "log", "redact");
        assert_eq!(flows, again);
    }

    #[test]
    fn one_source_reaching_a_sink_two_ways_is_deduplicated() {
        // Both branches alias the same `a = getSecret()` into `s`, so one (source, sink)
        // pair is reached by two chains. Path-insensitive union then dedup → a single flow.
        let src = "function f(c) { const a = getSecret(); let s; if (c) { s = a; } else { s = a; } log(s); }";
        let flows = run(src, "getSecret", "log", "redact");
        assert_eq!(
            flows.len(),
            1,
            "duplicate (source, sink) pairs collapse to one"
        );
        assert_eq!(flows[0].0, "getSecret()");
        assert_eq!(
            run(src, "getSecret", "log", "redact"),
            flows,
            "deterministic"
        );
    }

    #[test]
    fn steps_are_the_shortest_chain() {
        // When two def-use chains reach the sink, `steps` is the shortest; tie broken by
        // position. Here log(a) reads the source directly (empty steps) while log(b) reads
        // it through one alias (one step).
        let flows = run_full(
            "function f() { const a = getSecret(); const b = a; log(a); log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 2);
        // Canonical order is by sink position: log(a) precedes log(b).
        assert_eq!(flows[0].1, "a");
        assert_eq!(flows[0].2, 0, "log(a) reads the source directly");
        assert_eq!(flows[1].1, "b");
        assert_eq!(flows[1].2, 1, "log(b) reads it through one alias");
    }
}
