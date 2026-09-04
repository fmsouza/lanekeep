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
    ///
    /// **Takes exactly one block.** [`Cfg::blocks_of`] can resolve a construct to more
    /// than one copy — a `finally` body duplicated once per continuation (`cfg_build.rs`,
    /// Task 6) is the case that exists today — and each copy sits on only the subset of
    /// paths that reach the continuation it was built for. Calling this once on a single
    /// copy answers about that one block, and can answer `false` for **every** copy of a
    /// construct that genuinely runs on every path, because the copies partition the path
    /// set and no one of them covers all of it. A caller that needs "is this construct on
    /// every path" has to check every element of [`Cfg::blocks_of`] and combine the
    /// results — this method cannot be asked that question directly.
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
                // Marked on push rather than on pop: once a block is queued, nothing
                // will queue it again, so no block is ever pushed more than once. The
                // walk terminates either way — that comes from the visited set existing
                // at all, not from when it is written — but marking on pop would let a
                // block sit on the stack multiple times before its first pop, redoing its
                // successor scan once per duplicate.
                visited[target.index()] = true;
                stack.push(target);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::cfg::testing::{find, find_all, parse};
    use crate::cfg::{BlockId, Cfg};

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

    #[test]
    fn a_duplicated_finalizer_is_not_on_all_paths_through_any_one_copy() {
        // T7-R5: the coordinator's own example for this ruling was
        // `try { if (a) { return 1; } else { throw x; } } finally { c(); }`, which turns
        // out to build *one* copy, not two — both branches diverge to the same
        // continuation (`exit`), and `emit_finally_copy` memoizes on the continuation, so
        // the second call to `unwind` hits the memo and reuses the first copy. Verified by
        // instrumenting `Cfg::build` on that exact source before writing this fixture.
        //
        // Two continuations need two genuinely different destinations. An `if` with no
        // `else` gives that: the `throw` leaves through `exit` directly, and falling off
        // the end of the `if` is *normal completion* of the `try` body, which
        // `try_statement` unwinds separately to its own `after` block (Task 6's
        // `the_finally_is_copied_once_per_continuation_not_once_per_exit` already
        // establishes the same split, with two `return`s standing in for the throw).
        let source = "function f(a) { try { if (a) { throw x; } } finally { c(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let copies = cfg.blocks_of(
            find_all(&tree, "expression_statement")
                .into_iter()
                .find(|n| &source[n.byte_range()] == "c();")
                .unwrap(),
        );
        assert_eq!(
            copies.len(),
            2,
            "one copy for the throw's continuation, one for normal completion's",
        );

        // Neither copy alone is on every path: the throw path avoids the
        // normal-completion copy, and the normal-completion path avoids the throw's copy.
        for &copy in &copies {
            assert!(
                !cfg.on_all_paths_from(cfg.entry(), copy),
                "block {copy:?} sits on only its own continuation's paths, not on every path",
            );
        }

        // What *is* true is a fact about the set: no path avoids every copy at once. This
        // is the property `on_all_paths_from` cannot express (it takes one block), which
        // is exactly why `blocks_of` exists — a caller checks this itself, over every
        // element, rather than being able to ask the query for it directly.
        assert!(
            !avoids_every(&cfg, cfg.entry(), cfg.exit(), &copies),
            "every path must pass through at least one copy of the finalizer",
        );
    }

    /// Whether some path from `from` to `to` avoids every block in `avoided`.
    ///
    /// A test-only generalization of `walk`'s single-`avoid` case to a set, for asserting
    /// the property `on_all_paths_from` cannot: that a *group* of blocks together, not any
    /// one of them, is what sits on every path. Same shape as `walk` — explicit stack,
    /// visited bitset — for the same reason: no hash container, and no dependence on the
    /// order edges happened to be added in.
    fn avoids_every(cfg: &Cfg<'_>, from: BlockId, to: BlockId, avoided: &[BlockId]) -> bool {
        let mut visited = vec![false; cfg.blocks().count()];
        let mut stack = vec![from];
        visited[from.index()] = true;
        while let Some(id) = stack.pop() {
            if id == to {
                return true;
            }
            let mut next: Vec<BlockId> = cfg
                .block(id)
                .successors
                .iter()
                .map(|edge| edge.target)
                .collect();
            next.sort_unstable();
            for target in next {
                if avoided.contains(&target) || visited[target.index()] {
                    continue;
                }
                visited[target.index()] = true;
                stack.push(target);
            }
        }
        false
    }
}
