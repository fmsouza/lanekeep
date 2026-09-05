//! The taint (data-flow) analysis for TypeScript, TSX and JavaScript.
//!
//! A forward may-taint over the per-function [`Cfg`](crate::cfg::Cfg): a `@source` value
//! that reaches a `@sink` with no intervening `@sanitizer` is one [`FlowPath`]. The
//! analysis is intra-procedural (v1), value-level, and path-insensitive — see
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
            let cfg = enclosing_cfg(source, sink);
            let sink_block = cfg.as_ref().and_then(|cfg| cfg.block_of(sink));
            let taint = Taint {
                tree,
                source,
                sources,
                sanitizers,
                cfg: cfg.as_ref(),
            };
            for fact in taint.taint_of(sink, sink_block, 0) {
                flows.push(FlowPath {
                    source: fact.source,
                    sink,
                    steps: fact.steps,
                });
            }
        }
        flows
    }
}

/// One reason a value is tainted: the originating source, and the alias hops between it and
/// the value, in flow order (source → value).
struct Fact<'t> {
    source: Node<'t>,
    steps: Vec<Node<'t>>,
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
        // A sanitizer's result is clean — cut the value — before anything it contains is
        // considered.
        if is_member(expr, self.sanitizers) {
            return Vec::new();
        }
        // A direct source: the expression is a `@source` call itself.
        if is_member(expr, self.sources) {
            return vec![Fact {
                source: expr,
                steps: Vec::new(),
            }];
        }
        match expr.kind() {
            "identifier" => self.taint_of_identifier(expr, sink_block, depth),
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
        for def in definitions_of(decl) {
            if !self.reaches_sink(def, sink_block) {
                continue;
            }
            let alias = def.kind() == "identifier";
            for mut fact in self.taint_of(def, sink_block, depth.saturating_add(1)) {
                if alias {
                    // A `const b = a` hop is one step; a direct source assignment adds none.
                    fact.steps.push(def);
                }
                facts.push(fact);
            }
        }
        facts
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

/// The right-hand-side expressions that define the binding `decl` declares. For 4b this is
/// the declarator's own initializer; later sub-tasks add reassignments.
fn definitions_of(decl: Node<'_>) -> Vec<Node<'_>> {
    let mut defs = Vec::new();
    if decl.kind() == "variable_declarator"
        && let Some(value) = decl.child_by_field_name("value")
    {
        defs.push(value);
    }
    defs
}

/// Whether `node` is one of `set`, by tree-unique node identity.
fn is_member(node: Node<'_>, set: &[Node<'_>]) -> bool {
    set.iter().any(|member| member.id() == node.id())
}

/// The nearest enclosing function's CFG, walking ancestors until [`Cfg::build`] accepts one
/// as a root kind. `program` is a root, so a match is always found for a node in a parsed
/// tree.
fn enclosing_cfg<'t>(source: &str, node: Node<'t>) -> Option<Cfg<'t>> {
    let mut current = Some(node);
    while let Some(node) = current {
        if let Some(cfg) = Cfg::build(source, node) {
            return Some(cfg);
        }
        current = node.parent();
    }
    None
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
}
