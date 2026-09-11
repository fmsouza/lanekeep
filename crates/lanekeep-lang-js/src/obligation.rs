//! lang-js's implementation of the obligation capability, over the per-function CFG.

use lanekeep_lang::obligation::{Keyed, ObligationAnalyzer, ObligationScope, UnmetObligation};
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
        keyed: bool,
        acquires: &[Keyed<'t>],
        releases: &[Keyed<'t>],
    ) -> Vec<UnmetObligation<'t>> {
        let mut out: Vec<UnmetObligation<'t>> = Vec::new();

        // Source order of the acquire, for determinism.
        let mut ordered: Vec<Keyed<'t>> = acquires.to_vec();
        ordered.sort_by_key(|k| k.node.start_byte());

        for acquire in ordered {
            let Some(root) = enclosing_cfg_root(acquire.node) else {
                continue;
            };
            let Some(cfg) = Cfg::build(source, root) else {
                continue;
            };
            // `acquire` is typically a `call_expression` nested inside a statement, and
            // `cfg_build` only attributes whole statements — `block_of` alone would find
            // nothing for it. `resolve_block` walks up to the nearest attributed ancestor.
            let Some(acq_block) = resolve_block(&cfg, acquire.node) else {
                continue;
            };

            // `scope: 'block'` additionally restricts discharge to a release lexically
            // inside the enclosing `statement_block` — `on_all_paths_within`'s own region.
            // With no enclosing block (top-level code), the obligation falls back to the
            // function frame, same as `scope: 'function'`. `Module` is not yet given its own
            // region — it is carried here alongside `Function` until a later task gives it
            // real, file-wide, key-correlated discharge.
            let region = match scope {
                ObligationScope::Block => {
                    enclosing_block(acquire.node).map(|block| block.byte_range())
                }
                ObligationScope::Function | ObligationScope::Module => None,
            };

            // Value identity: with `keyed`, a release only discharges an acquire whose
            // `@key` text agrees with its own. A keyed acquire whose match bound no `@key`
            // correlates with nothing, so it is always reported (the `_ => false` arm).
            let key_text = |k: &Keyed<'t>| k.key.map(|n| &source[n.byte_range()]);
            let acq_key = key_text(&acquire);

            // Release blocks for this same function (a release node whose root is this
            // root) and, for block scope, lexically inside `region`. A release outside the
            // region cannot be what discharges a block-scoped obligation even if it is
            // reachable — `{ acquire(); } release();` must still report. `resolve_blocks`
            // handles finally-duplicated release nodes.
            let rel_blocks: Vec<BlockId> = releases
                .iter()
                .filter(|r| enclosing_cfg_root(r.node).is_some_and(|rr| rr.id() == root.id()))
                .filter(|r| {
                    region.as_ref().is_none_or(|region| {
                        r.node.start_byte() >= region.start && r.node.end_byte() <= region.end
                    })
                })
                .filter(|r| {
                    if !keyed {
                        return true; // un-keyed: today's behavior, any release discharges.
                    }
                    // keyed: both sides must carry a key and the texts must agree.
                    match (acq_key, key_text(r)) {
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    }
                })
                .flat_map(|r| resolve_blocks(&cfg, r.node))
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
                .unwrap_or(acquire.node);

            // partial: some path did discharge, i.e. a release is reachable at all.
            let partial = rel_blocks.iter().any(|&r| cfg.reaches(acq_block, r));

            out.push(UnmetObligation {
                acquire: acquire.node,
                exit: witness,
                partial,
                key: acquire.key,
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
    use lanekeep_lang::obligation::{Keyed, ObligationAnalyzer, ObligationScope};

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

    /// Wrap bare nodes for a signature that now takes `&[Keyed]` everywhere, with no `@key`
    /// bound — every fixture in this file predates value-identity and asserts the unkeyed
    /// behavior, which this task must leave unchanged.
    fn bare<'t>(ns: Vec<tree_sitter::Node<'t>>) -> Vec<Keyed<'t>> {
        ns.into_iter()
            .map(|node| Keyed { node, key: None })
            .collect()
    }

    #[test]
    fn zeroed_on_all_paths_is_silent() {
        let source = "function f() { const b = acq(); rel(b); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            false,
            &bare(acq),
            &bare(rel),
        );
        assert!(unmet.is_empty());
    }

    #[test]
    fn missed_on_an_early_return_reports_partial() {
        let source = "function f(c) { const b = acq(); if (c) { return; } rel(b); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            false,
            &bare(acq),
            &bare(rel),
        );
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].partial, "the fallthrough path did discharge");
        assert_eq!(unmet[0].exit.kind(), "return_statement");
    }

    #[test]
    fn never_zeroed_reports_not_partial() {
        let source = "function f() { const b = acq(); }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            false,
            &bare(acq),
            &[],
        );
        assert_eq!(unmet.len(), 1);
        assert!(!unmet[0].partial);
    }

    #[test]
    fn a_finally_release_is_silent() {
        let source = "function f() { const b = acq(); try { use(b); } finally { rel(b); } }";
        let tree = parse(source);
        let acq = calls(&tree, source, "acq()");
        let rel = calls(&tree, source, "rel(b)");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            false,
            &bare(acq),
            &bare(rel),
        );
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
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Block,
            false,
            &bare(acq),
            &bare(rel),
        );
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
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Block,
            false,
            &bare(acq),
            &bare(rel),
        );
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

    /// Build `Keyed` pairs whose `key` is the first argument identifier of each matched
    /// call node — e.g. `acq(a)` / `rel(a)` -> the `a`. Confirmed against the parser
    /// directly (not just `node-types.json`): `call_expression.arguments` is the
    /// `arguments` node, and its first named child is the bare argument expression with
    /// no intervening wrapper.
    fn keyed_calls<'t>(tree: &'t tree_sitter::Tree, source: &str, text: &str) -> Vec<Keyed<'t>> {
        calls(tree, source, text)
            .into_iter()
            .map(|node| {
                let key = node
                    .child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0));
                Keyed { node, key }
            })
            .collect()
    }

    #[test]
    fn keyed_function_scope_discharges_only_the_matching_acquire() {
        // rel(a) releases `a`, not `b`: `b` must be reported, `a` silent.
        let source = "function f() { const a = acq(a); const b = acq(b); rel(a); }";
        let tree = parse(source);
        let acq = keyed_calls(&tree, source, "acq(a)")
            .into_iter()
            .chain(keyed_calls(&tree, source, "acq(b)"))
            .collect::<Vec<_>>();
        let rel = keyed_calls(&tree, source, "rel(a)");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            true,
            &acq,
            &rel,
        );
        assert_eq!(unmet.len(), 1, "only b is undischarged");
        assert_eq!(&source[unmet[0].acquire.byte_range()], "acq(b)");
    }

    #[test]
    fn keyed_acquire_without_a_captured_key_is_reported() {
        // keyed obligation, but this acquire bound no @key -> cannot correlate -> reported.
        let source = "function f() { const a = acq(); rel(a); }";
        let tree = parse(source);
        let acq = vec![Keyed {
            node: calls(&tree, source, "acq()")[0],
            key: None,
        }];
        let rel = keyed_calls(&tree, source, "rel(a)");
        let unmet = JsObligationAnalyzer.analyze(
            &tree,
            source,
            ObligationScope::Function,
            true,
            &acq,
            &rel,
        );
        assert_eq!(
            unmet.len(),
            1,
            "no key means no correlation, so it is reported"
        );
    }

    #[test]
    fn keyed_block_scope_requires_both_region_and_key_match() {
        // `a` is released inside its block with the matching key: discharged. `b`'s only
        // release shares its key but sits outside the block, lexically — block scope
        // must still report it even though a keyed match for it exists in the function.
        let source = "function f() { { const a = acq(a); const b = acq(b); rel(a); } rel(b); }";
        let tree = parse(source);
        let acq = keyed_calls(&tree, source, "acq(a)")
            .into_iter()
            .chain(keyed_calls(&tree, source, "acq(b)"))
            .collect::<Vec<_>>();
        let rel = keyed_calls(&tree, source, "rel(a)")
            .into_iter()
            .chain(keyed_calls(&tree, source, "rel(b)"))
            .collect::<Vec<_>>();
        let unmet =
            JsObligationAnalyzer.analyze(&tree, source, ObligationScope::Block, true, &acq, &rel);
        assert_eq!(
            unmet.len(),
            1,
            "b's matching-key release is lexically outside its block"
        );
        assert_eq!(&source[unmet[0].acquire.byte_range()], "acq(b)");
    }
}
