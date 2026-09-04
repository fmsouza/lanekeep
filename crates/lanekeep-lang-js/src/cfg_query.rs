//! Reachability over a [`Cfg`].
//!
//! Two questions, and neither is a dataflow analysis: *may* control get from here to
//! there, and *must* it pass through a given block on the way out. lanekeep #194's taint
//! is the first consumer of the may-question and #193's obligation of the must-question.
//!
//! Both walks use an explicit stack with a visited set, pushing successors in
//! [`BlockId`] order. No hash container is iterated anywhere in this file — which is the
//! checkable form of the determinism requirement, since an arbitrary iteration order
//! would make output vary between runs on identical input.

use crate::cfg::{BlockId, Cfg};

impl Cfg<'_> {
    /// Whether control can reach `to` from `from`.
    ///
    /// Reflexive: a block reaches itself.
    #[must_use]
    pub fn reaches(&self, from: BlockId, to: BlockId) -> bool {
        self.walk(from, None, to)
    }

    /// Whether **every** path from `from` to [`Cfg::exit`] passes through `to`.
    ///
    /// Delete `to` and ask whether the exit is still reachable: if it is, that walk is a
    /// witness to a path that avoids `to`. This is the definition restated rather than a
    /// dominator algorithm to re-derive when it is wrong.
    ///
    /// **Vacuously true when the exit is unreachable from `from`** — `from` inside an
    /// infinite loop — because there are then no paths for a witness to be found on. That
    /// is the correct must-semantics and it is not what a reader guesses, which is why it
    /// is written here.
    #[must_use]
    pub fn on_all_paths_from(&self, from: BlockId, to: BlockId) -> bool {
        // Redundant with `walk`'s own first line: when `from == to`, `avoid == Some(from)`,
        // so `walk` immediately hits `Some(from) == avoid` and returns `false`, which this
        // function negates to `true` — the same answer this early return gives, for every
        // input. No fixture can distinguish "guard present" from "guard deleted", so this
        // is not untested code waiting for a test that would catch its removal; there is no
        // such test. Kept anyway, because the reflexive case should be legible at the call
        // site rather than emergent two functions away.
        if from == to {
            return true;
        }
        !self.walk(from, Some(to), self.exit)
    }

    /// Depth-first from `from`, never entering `avoid`, stopping at `goal`.
    fn walk(&self, from: BlockId, avoid: Option<BlockId>, goal: BlockId) -> bool {
        if Some(from) == avoid {
            return false;
        }
        let mut visited = vec![false; self.blocks.len()];
        let mut stack = vec![from];
        visited[from.index()] = true;
        while let Some(id) = stack.pop() {
            if id == goal {
                return true;
            }
            // Successors in `BlockId` order, so the traversal does not depend on the
            // order construction happened to add edges in.
            let mut next: Vec<BlockId> = self.blocks[id.index()]
                .successors
                .iter()
                .map(|edge| edge.target)
                .collect();
            next.sort_unstable();
            for target in next {
                if Some(target) == avoid || visited[target.index()] {
                    continue;
                }
                // Marked on push rather than on pop: a back edge then terminates the walk
                // instead of re-entering the loop body forever.
                visited[target.index()] = true;
                stack.push(target);
            }
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use crate::cfg::Cfg;
    use crate::cfg::testing::{find, find_all, parse};

    fn build<'t>(tree: &'t tree_sitter::Tree, source: &str) -> Cfg<'t> {
        Cfg::build(source, find(tree, "function_declaration")).expect("a root")
    }

    #[test]
    fn the_exit_is_reachable_from_the_entry() {
        let source = "function f() { a(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.reaches(cfg.entry(), cfg.exit()));
    }

    #[test]
    fn reaches_is_reflexive() {
        let source = "function f() { a(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.reaches(cfg.entry(), cfg.entry()));
    }

    #[test]
    fn an_unreachable_block_is_not_reached() {
        // Not dead code trailing a `return`: `statements`'s `?` on `self.statement(...)`
        // bails out of the loop over the parent's children before the next sibling is
        // even visited, so nothing would ever be attributed for `block_of` to resolve —
        // `cfg_build.rs`'s own `both_arms_returning_leaves_the_join_unreachable` pins
        // exactly that with `block_of(..).is_none()`. A conditionless `for` is the one
        // construct whose `after` block is walked regardless of reachability
        // (`loop_statement`'s own doc comment: "reachability is a fact about `after`'s
        // predecessor count, not about this return value"), so code following a loop with
        // no exit is real, attributed, and genuinely unreachable — the same fixture shape
        // as `cfg_build.rs`'s `a_conditionless_for_loop_has_no_way_to_fall_through`.
        let source = "function f() { for (;;) { a(); } dead(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let dead = cfg
            .block_of(
                find_all(&tree, "expression_statement")
                    .into_iter()
                    .find(|n| &source[n.byte_range()] == "dead();")
                    .unwrap(),
            )
            .unwrap();
        assert!(!cfg.reaches(cfg.entry(), dead));
    }

    #[test]
    fn a_loop_back_edge_terminates_the_walk() {
        // The assertion that matters is that this returns at all.
        let source = "function f() { while (c) { a(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.reaches(cfg.entry(), cfg.exit()));
    }

    #[test]
    fn a_finally_is_on_all_paths_out_of_its_try() {
        let source = "function f() { try { return 1; } finally { c(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let cleanup = cfg
            .block_of(
                find_all(&tree, "call_expression")
                    .into_iter()
                    .find(|n| &source[n.byte_range()] == "c()")
                    .unwrap(),
            )
            .unwrap();
        assert!(cfg.on_all_paths_from(cfg.entry(), cleanup));
    }

    #[test]
    fn a_conditional_cleanup_is_not_on_all_paths() {
        let source = "function f() { if (c) { z(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let cleanup = cfg.block_of(find(&tree, "expression_statement")).unwrap();
        assert!(!cfg.on_all_paths_from(cfg.entry(), cleanup));
    }

    #[test]
    fn on_all_paths_from_is_reflexive() {
        let source = "function f() { a(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.on_all_paths_from(cfg.entry(), cfg.entry()));
    }

    #[test]
    fn the_exit_is_on_every_path_to_the_exit() {
        let source = "function f() { if (c) { a(); } else { b(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.on_all_paths_from(cfg.entry(), cfg.exit()));
    }

    #[test]
    fn an_infinite_loop_makes_everything_vacuously_on_all_paths() {
        // Documented surprise, asserted so it cannot change silently: with no path to the
        // exit there is no witness, so the answer is `true`.
        // `for (;;)` and not `while (true)`: there is no constant folding here, so a
        // `while` with a condition keeps its `False` edge and the exit stays reachable.
        // A `for` with no test has no such edge, which is the only genuinely exit-less
        // loop this construction produces.
        let source = "function f() { for (;;) { a(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let inside = cfg.block_of(find(&tree, "expression_statement")).unwrap();
        assert!(!cfg.reaches(inside, cfg.exit()), "for(;;) has no way out");
        assert!(
            cfg.on_all_paths_from(inside, cfg.entry()),
            "with no path to the exit there is no witness, so the answer is vacuously true",
        );
    }
}
