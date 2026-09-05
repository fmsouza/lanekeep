//! The taint (data-flow) analysis capability: a *may*-question over a control-flow graph —
//! can a tainted value reach a sink without passing a sanitizer.
//!
//! This crate owns only the trait and its data; the analysis is per-language (lang-js today),
//! exposed through [`crate::Language::flow_analyzer`] exactly as binding resolution is through
//! [`crate::Language::resolver`].

use tree_sitter::{Node, Tree};

/// One tainted flow: a source whose value reaches a sink with no intervening sanitizer.
#[derive(Debug, Clone)]
pub struct FlowPath<'t> {
    /// The `@source` node the tainted value originates at.
    pub source: Node<'t>,
    /// The `@sink` node the tainted value reaches.
    pub sink: Node<'t>,
    /// The assignments and calls between source and sink, in flow order. One canonical path.
    pub steps: Vec<Node<'t>>,
}

/// A per-language may-taint analysis over source/sink/sanitizer node sets.
pub trait FlowAnalyzer: Send + Sync {
    /// One [`FlowPath`] per (source, sink) where taint reaches the sink without passing a
    /// sanitizer, deduplicated, in canonical order (sink source-position, then source).
    fn analyze<'t>(
        &self,
        tree: &'t Tree,
        source: &str,
        sources: &[Node<'t>],
        sinks: &[Node<'t>],
        sanitizers: &[Node<'t>],
    ) -> Vec<FlowPath<'t>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub analyzer for testing the trait's existence.
    struct StubAnalyzer;

    impl FlowAnalyzer for StubAnalyzer {
        fn analyze<'t>(
            &self,
            _tree: &'t Tree,
            _source: &str,
            _sources: &[Node<'t>],
            _sinks: &[Node<'t>],
            _sanitizers: &[Node<'t>],
        ) -> Vec<FlowPath<'t>> {
            vec![]
        }
    }

    #[test]
    fn flow_analyzer_can_be_implemented() {
        use std::sync::Arc;
        // If this compiles, FlowAnalyzer trait is object-safe and correctly defined.
        drop(Arc::new(StubAnalyzer) as Arc<dyn FlowAnalyzer>);
    }
}
