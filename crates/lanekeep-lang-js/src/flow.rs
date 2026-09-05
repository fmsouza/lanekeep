//! The taint (data-flow) analysis for TypeScript, TSX and JavaScript.
//!
//! A forward may-taint over the per-function [`Cfg`](crate::cfg::Cfg): a `@source` value
//! that reaches a `@sink` with no intervening `@sanitizer` is one [`FlowPath`]. The
//! analysis is intra-procedural (v1), value-level, and path-insensitive — see
//! `docs/superpowers/specs/2026-09-05-taint-analysis-flow-checkflow-design.md` §5.

use lanekeep_lang::flow::{FlowAnalyzer, FlowPath};
use tree_sitter::{Node, Tree};

/// The JS/TS taint analyzer, returned from [`crate::TypeScript::flow_analyzer`] and its
/// siblings.
pub(crate) struct JsFlowAnalyzer;

impl FlowAnalyzer for JsFlowAnalyzer {
    fn analyze<'t>(
        &self,
        _tree: &'t Tree,
        _source: &str,
        sources: &[Node<'t>],
        sinks: &[Node<'t>],
        _sanitizers: &[Node<'t>],
    ) -> Vec<FlowPath<'t>> {
        let mut flows = Vec::new();
        for &sink in sinks {
            for &source in sources {
                // 4a: the sink argument *is* (or contains) the source call.
                if contains(sink, source) {
                    flows.push(FlowPath {
                        source,
                        sink,
                        steps: Vec::new(),
                    });
                }
            }
        }
        flows
    }
}

/// Whether `outer`'s byte range covers `inner`'s.
fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
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
}
