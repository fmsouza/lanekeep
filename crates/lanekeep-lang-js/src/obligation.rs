//! lang-js's implementation of the obligation capability, over the per-function CFG.

use lanekeep_lang::obligation::{ObligationAnalyzer, ObligationScope, UnmetObligation};
use tree_sitter::{Node, Tree};

use crate::cfg::{BlockId, Cfg};
use crate::cfg_build::{enclosing_block, enclosing_cfg_root};

/// Stateless; the shared static below is one instance for all three lang-js languages.
pub(crate) struct JsObligationAnalyzer;

impl ObligationAnalyzer for JsObligationAnalyzer {
    fn analyze<'t>(
        &self,
        _tree: &'t Tree,
        source: &str,
        scope: ObligationScope,
        acquires: &[Node<'t>],
        releases: &[Node<'t>],
    ) -> Vec<UnmetObligation<'t>> {
        let mut out: Vec<UnmetObligation<'t>> = Vec::new();

        // Source order of the acquire, for determinism.
        let mut ordered: Vec<Node<'t>> = acquires.to_vec();
        ordered.sort_by_key(Node::start_byte);

        for acquire in ordered {
            let Some(root) = enclosing_cfg_root(acquire) else {
                continue;
            };
            let Some(cfg) = Cfg::build(source, root) else {
                continue;
            };
            // `acquire` is typically a `call_expression` nested inside a statement, and
            // `cfg_build` only attributes whole statements — `block_of` alone would find
            // nothing for it. `resolve_block` walks up to the nearest attributed ancestor.
            let Some(acq_block) = resolve_block(&cfg, acquire) else {
                continue;
            };

            // `scope: 'block'` additionally restricts discharge to a release lexically
            // inside the enclosing `statement_block` — `on_all_paths_within`'s own region.
            // With no enclosing block (top-level code), the obligation falls back to the
            // function frame, same as `scope: 'function'`.
            let region = match scope {
                ObligationScope::Block => enclosing_block(acquire).map(|block| block.byte_range()),
                ObligationScope::Function => None,
            };

            // Release blocks for this same function (a release node whose root is this
            // root) and, for block scope, lexically inside `region`. A release outside the
            // region cannot be what discharges a block-scoped obligation even if it is
            // reachable — `{ acquire(); } release();` must still report. `resolve_blocks`
            // handles finally-duplicated release nodes.
            let rel_blocks: Vec<BlockId> = releases
                .iter()
                .filter(|r| enclosing_cfg_root(**r).is_some_and(|rr| rr.id() == root.id()))
                .filter(|r| {
                    region.as_ref().is_none_or(|region| {
                        r.start_byte() >= region.start && r.end_byte() <= region.end
                    })
                })
                .flat_map(|r| resolve_blocks(&cfg, *r))
                .collect();

            let discharged = match &region {
                Some(region) => cfg.on_all_paths_within(acq_block, region.clone(), &rel_blocks),
                None => cfg.on_all_paths_from_any(acq_block, &rel_blocks),
            };
            if discharged {
                continue;
            }

            // Witness exit: the source-earliest exit reachable from the acquire while
            // avoiding every release. `exits()` is already in source order.
            let witness = cfg
                .exits()
                .into_iter()
                .filter(|e| cfg.reaches_avoiding(acq_block, &rel_blocks, e.block))
                .find_map(|e| e.node)
                // No concrete return/throw on the escaping path: report at the acquire.
                .unwrap_or(acquire);

            // partial: some path did discharge, i.e. a release is reachable at all.
            let partial = rel_blocks.iter().any(|&r| cfg.reaches(acq_block, r));

            out.push(UnmetObligation {
                acquire,
                exit: witness,
                partial,
            });
        }
        out
    }
}

/// Resolve `node` to the block that contains it, walking up to the nearest ancestor
/// [`Cfg::block_of`] can answer for.
///
/// `block_of` resolves a node `cfg_build` attributed directly, plus containment fallback —
/// but the acquire capture from a query is typically a `call_expression` nested inside a
/// statement, which is attributed nowhere on its own. Walking to the nearest ancestor that
/// does resolve is bounded within the enclosing function: `block_of` refuses any node
/// outside `root`'s own byte range, so the walk cannot silently cross into an enclosing
/// scope's graph.
fn resolve_block<'t>(cfg: &Cfg<'t>, node: Node<'t>) -> Option<BlockId> {
    let mut current = Some(node);
    while let Some(n) = current {
        if let Some(block) = cfg.block_of(n) {
            return Some(block);
        }
        current = n.parent();
    }
    None
}

/// The [`resolve_block`] analogue for [`Cfg::blocks_of`], used for a release.
///
/// Unlike an acquire, a release can legitimately resolve to more than one block: a release
/// inside a `finally` body is attributed once per continuation `cfg_build` duplicates it
/// for, and every copy has to count toward discharge.
fn resolve_blocks<'t>(cfg: &Cfg<'t>, node: Node<'t>) -> Vec<BlockId> {
    let mut current = Some(node);
    while let Some(n) = current {
        let blocks = cfg.blocks_of(n);
        if !blocks.is_empty() {
            return blocks;
        }
        current = n.parent();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::JsObligationAnalyzer;
    use crate::cfg::testing::{find_all, parse};
    use lanekeep_lang::obligation::{ObligationAnalyzer, ObligationScope};

    fn calls<'t>(
        tree: &'t tree_sitter::Tree,
        source: &str,
        text: &str,
    ) -> Vec<tree_sitter::Node<'t>> {
        find_all(tree, "call_expression")
            .into_iter()
            .filter(|n| &source[n.byte_range()] == text)
            .collect()
    }

    #[test]
    fn zeroed_on_all_paths_is_silent() {
        let source = "function f() { const b = acq(); rel(b); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet =
            JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Function, &acq, &rel);
        assert!(unmet.is_empty());
    }

    #[test]
    fn missed_on_an_early_return_reports_partial() {
        let source = "function f(c) { const b = acq(); if (c) { return; } rel(b); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet =
            JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Function, &acq, &rel);
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].partial, "the fallthrough path did discharge");
        assert_eq!(unmet[0].exit.kind(), "return_statement");
    }

    #[test]
    fn never_zeroed_reports_not_partial() {
        let source = "function f() { const b = acq(); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let unmet =
            JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Function, &acq, &[]);
        assert_eq!(unmet.len(), 1);
        assert!(!unmet[0].partial);
    }

    #[test]
    fn a_finally_release_is_silent() {
        let source = "function f() { const b = acq(); try { use(b); } finally { rel(b); } }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet =
            JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Function, &acq, &rel);
        assert!(unmet.is_empty(), "finally is on all paths");
    }

    // The two hand-traced examples from the block-scope byte-range correction, each turned
    // into a real assertion rather than left as prose: `{ acq(); rel(); } after()` is
    // silent, `{ acq(); } rel()` reports — because the release sits lexically outside the
    // acquire's own block and so cannot be what discharges it there, even though nothing
    // in this fixture makes the two share a distinct control-flow block.

    #[test]
    fn block_scope_is_silent_when_the_release_is_inside_the_block() {
        let source = "function f() { { const b = acq(); rel(b); } after(); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet = JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Block, &acq, &rel);
        assert!(
            unmet.is_empty(),
            "the release is lexically inside the block"
        );
    }

    #[test]
    fn block_scope_reports_when_the_release_is_outside_the_block() {
        let source = "function f() { { const b = acq(); } rel(b); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet = JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Block, &acq, &rel);
        assert_eq!(
            unmet.len(),
            1,
            "the release is lexically outside the block, so it must not count"
        );
        assert!(
            !unmet[0].partial,
            "no in-scope path discharges it, so this is not a partial miss"
        );
    }
}
