//! Reachability over a [`Cfg`].
//!
//! Two questions, and neither is a dataflow analysis: *may* control get from here to
//! there, and *must* it pass through a given block on the way out. lanekeep #194's taint
//! is the first consumer of the may-question and #193's obligation of the must-question.
//!
//! The must-question comes in two shapes, one block and a set of them, and the set is not a
//! convenience over the single form: construction duplicates a `finally` body per
//! continuation, and the per-copy answers cannot be combined into the answer about the
//! construct. Both shapes are one walk with the same body.
//!
//! Both walks use an explicit stack with a visited set, pushing successors in
//! [`BlockId`] order. No hash container is iterated anywhere in this file — which is the
//! checkable form of the determinism requirement, since an arbitrary iteration order
//! would make output vary between runs on identical input.

use std::ops::Range;

use crate::cfg::{BlockId, Cfg};

impl Cfg<'_> {
    /// Whether control can reach `to` from `from`.
    ///
    /// Reflexive: a block reaches itself.
    ///
    /// # Panics
    ///
    /// Panics if `from` came from another function's graph and is out of range here; see
    /// [`Cfg::block`]. `from` is the only *parameter* the walk indexes — it also indexes
    /// each edge target, but those come from the graph itself and are in range by
    /// construction. `to` is only ever compared, so a foreign `to` answers `false` rather
    /// than panicking, which is the quieter and worse of the two failures.
    #[must_use]
    pub fn reaches(&self, from: BlockId, to: BlockId) -> bool {
        self.walk(from, &[], to)
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
    /// **Takes exactly one block**, so it is the wrong question for a construct that may
    /// be duplicated. [`Cfg::blocks_of`] can resolve one to several copies — a `finally`
    /// body emitted once per continuation (`cfg_build.rs`) is the case that exists today —
    /// and each copy sits on only the subset of paths reaching the continuation it was
    /// built for. Asked about one copy this answers about that one block, and it can
    /// answer `false` for **every** copy of a construct that genuinely runs on every path,
    /// because the copies partition the path set and no one of them covers all of it.
    ///
    /// **Combining those per-copy answers recovers nothing.** With `false` for each copy,
    /// the OR is `false` and the AND is `false`, while the truth is that the construct is
    /// on every path — no function of the per-copy booleans yields it, since none of them
    /// carries which paths its copy covers. Ask [`Cfg::on_all_paths_from_any`] instead: it
    /// takes the whole set and deletes it in one walk, which is the only form in which
    /// this question can be put about a duplicated construct.
    ///
    /// # Panics
    ///
    /// Panics if `from` came from another function's graph and is out of range here; see
    /// [`Cfg::block`]. `to` is only ever compared, never indexed, so a foreign one is not
    /// caught at all: it deletes no block, and the answer comes back as though nothing had
    /// been asked about.
    #[must_use]
    pub fn on_all_paths_from(&self, from: BlockId, to: BlockId) -> bool {
        // Redundant with `walk`'s own first line: when `from == to`, `avoid` holds `from`,
        // so `walk` immediately hits `avoid.contains(&from)` and returns `false`, which
        // this function negates to `true` — the same answer this early return gives, for
        // every input. No fixture can distinguish "guard present" from "guard deleted"
        // *through this method*; `on_all_paths_from_any_is_true_when_from_is_in_the_set`
        // covers `walk`'s line itself, which has no such shadowing guard above it. Kept
        // anyway, because the reflexive case should be legible at the call site rather
        // than emergent two functions away.
        if from == to {
            return true;
        }
        !self.walk(from, &[to], self.exit)
    }

    /// Whether control can reach `to` from `from` without entering any block in `avoid`.
    ///
    /// The public form of the private `walk`, for a consumer that needs the witness
    /// path rather than the all-paths verdict — obligation analysis, to name the exit an
    /// undischarged value escapes through.
    #[must_use]
    pub fn reaches_avoiding(&self, from: BlockId, avoid: &[BlockId], to: BlockId) -> bool {
        self.walk(from, avoid, to)
    }

    /// Whether every path from `from` to [`Cfg::exit`] passes through **at least one** of
    /// `through`.
    ///
    /// The set form of [`Cfg::on_all_paths_from`], and the method to reach for when
    /// [`Cfg::blocks_of`] answers with more than one block. Same walk, avoiding the whole
    /// set at once: delete every element and ask whether the exit is still reachable, in
    /// which case that walk is a witness to a path taking none of them.
    ///
    /// A duplicated `finally` is the reason this exists rather than a generalization for
    /// its own sake. Each copy carries only the paths reaching its own continuation, so
    /// the single form answers `false` for every copy of a finalizer that nonetheless runs
    /// on every path — and no combination of those `false`s recovers the truth. The
    /// property is a fact about the copies *together*, which is a question only a set can
    /// put.
    ///
    /// The single form's two edge cases hold here unchanged, plus one this shape adds:
    ///
    /// - **Vacuously true when the exit is unreachable from `from`** — `from` inside an
    ///   infinite loop — because there are then no paths for a witness to be found on.
    /// - **True when `from` is itself in `through`**, since every path from `from` starts
    ///   there.
    /// - An empty `through` is therefore `false` whenever the exit is reachable at all,
    ///   and vacuously `true` when it is not. Nothing is on a path that does not exist.
    ///
    /// # Panics
    ///
    /// Panics if `from` came from another function's graph and is out of range here; see
    /// [`Cfg::block`]. Elements of `through` are only ever compared, so foreign ones delete
    /// nothing and this answers `false` for a construct that is on every path — a set makes
    /// that easier to do by accident than the single form does, since the ids arrive as a
    /// collection built somewhere else.
    #[must_use]
    pub fn on_all_paths_from_any(&self, from: BlockId, through: &[BlockId]) -> bool {
        !self.walk(from, through, self.exit)
    }

    /// Whether every path leaving `region` from `from` passes through one of `through`.
    ///
    /// The block-scope analogue of [`Self::on_all_paths_from_any`], whose goal is the
    /// function exit. Here the goal is the region's frontier — the first block on each path
    /// whose `start` byte is outside `region`. If some frontier block is reachable from
    /// `from` while avoiding every `through` block, that walk witnesses a path out of the
    /// region that discharges nothing.
    ///
    /// **A `through` block only counts once it is confirmed still inside `region`.** Checking
    /// set membership before checking position would credit a release block that has itself
    /// already crossed the frontier — exactly the case a block-scoped obligation must reject,
    /// since a release lexically outside the block cannot be what discharged it. `from` is
    /// exempted from the position check (it is where the acquire sits, not where its
    /// enclosing block starts, so its own `start` byte is typically before `region`), matching
    /// `walk`'s treatment of its own starting block.
    ///
    /// # Panics
    ///
    /// Panics if `from` came from another function's graph and is out of range here; see
    /// [`Cfg::block`]. Elements of `through` are only ever compared, so a foreign one
    /// avoids nothing.
    #[must_use]
    pub fn on_all_paths_within(
        &self,
        from: BlockId,
        region: Range<usize>,
        through: &[BlockId],
    ) -> bool {
        // Successors in `BlockId` order; explicit stack; no hash container iterated —
        // same discipline as `walk`, restated rather than shared because the stopping
        // condition here is a predicate over the region, not a single goal block.
        let mut visited = vec![false; self.blocks.len()];
        let mut stack = vec![from];
        visited[from.index()] = true;
        while let Some(id) = stack.pop() {
            let block = self.block(id);
            let left_region = id != from && !region.contains(&block.start);
            if through.contains(&id) && !left_region {
                continue; // discharged, and it happened before leaving the region
            }
            if left_region {
                return false; // left the region without passing a release still inside it
            }
            let mut next: Vec<BlockId> = block.successors.iter().map(|edge| edge.target).collect();
            next.sort_unstable();
            for target in next {
                if !visited[target.index()] {
                    visited[target.index()] = true;
                    stack.push(target);
                }
            }
        }
        true
    }

    /// Depth-first from `from`, never entering a block in `avoid`, stopping at `goal`.
    ///
    /// `avoid` is a slice rather than a set container because it holds a handful of blocks
    /// — the copies of one construct — so a linear scan beats hashing outright. Not for the
    /// determinism requirement: this file's header forbids *iterating* a hash container, and
    /// a `HashSet` read only through `contains` would not breach that. Size is the whole
    /// reason.
    fn walk(&self, from: BlockId, avoid: &[BlockId], goal: BlockId) -> bool {
        if avoid.contains(&from) {
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
                if avoid.contains(&target) || visited[target.index()] {
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
    fn a_duplicated_finalizer_is_on_all_paths_only_as_a_set() {
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

        // Both halves of the contrast, in one place, because either alone reads as a
        // property of this fixture rather than as the reason the set form exists.
        //
        // Half one: neither copy alone is on every path. The throw path avoids the
        // normal-completion copy, and the normal-completion path avoids the throw's copy.
        for &copy in &copies {
            assert!(
                !cfg.on_all_paths_from(cfg.entry(), copy),
                "block {copy:?} sits on only its own continuation's paths, not on every path",
            );
        }

        // Half two: the finalizer nonetheless runs on every path, and `on_all_paths_from_any`
        // is what says so. Note what half one rules out — every per-copy answer is `false`,
        // so an OR over them is `false` and an AND over them is `false`, and neither is the
        // `true` below. There is no function of the per-copy booleans that gets here; the
        // set has to enter the walk as a set.
        assert!(
            cfg.on_all_paths_from_any(cfg.entry(), &copies),
            "every path must pass through at least one copy of the finalizer",
        );
    }

    #[test]
    fn on_all_paths_from_any_is_true_when_from_is_in_the_set() {
        // `walk`'s own first line, and the only test that reaches it: `on_all_paths_from`
        // has a reflexive early return of its own that answers before the walk is ever
        // called, so deleting that line is invisible through the single form. Here nothing
        // shadows it — with it gone the walk starts inside the avoided set, reaches the
        // exit, and the answer flips to `false`.
        let source = "function f() { a(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        assert!(cfg.on_all_paths_from_any(cfg.entry(), &[cfg.entry()]));
    }

    #[test]
    fn on_all_paths_from_any_is_false_when_one_path_avoids_every_block() {
        // The negative direction, which the duplicated-finalizer test above cannot give:
        // every assertion there is that some block or set *is* on every path, so a body of
        // `true` would satisfy all of them. Here the `if` supplies a path around the whole
        // set — one block in this fixture, since nothing duplicates it.
        let source = "function f(c) { if (c) { z(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let cleanup = cfg.blocks_of(find(&tree, "expression_statement"));
        assert_eq!(cleanup.len(), 1, "nothing duplicates a plain `if` body");
        assert!(!cfg.on_all_paths_from_any(cfg.entry(), &cleanup));
        assert!(
            !cfg.on_all_paths_from_any(cfg.entry(), &[]),
            "an empty set is on no path at all while the exit is reachable",
        );
    }

    #[test]
    fn reaches_avoiding_is_blocked_by_the_avoided_set() {
        // acquire -> (release) -> exit ; avoiding the release, the exit is unreachable
        let source = "function f() { a(); r(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let release = cfg
            .block_of(
                find_all(&tree, "call_expression")
                    .into_iter()
                    .find(|n| &source[n.byte_range()] == "r()")
                    .unwrap(),
            )
            .unwrap();
        assert!(cfg.reaches_avoiding(cfg.entry(), &[], cfg.exit()));
        assert!(!cfg.reaches_avoiding(cfg.entry(), &[release], cfg.exit()));
    }

    /// Resolves an `acquire`/`release`/`enclosing_block` triple for the two fixtures below.
    ///
    /// The `release` is put inside a `switch` with only a `default` arm — the one construct
    /// that reaches an inner statement through a single, *unconditional* edge
    /// (`switch_statement`'s own doc comment: zero `switch_case` arms means the discriminant
    /// reaches `default` directly) while still allocating it a block of its own. A bare
    /// `{ a(); r(); }` does not: `cfg_build.rs`'s `"statement_block" => self.statements(...)`
    /// has no `alloc` of its own, so a nested block with nothing branching inside it merges
    /// straight into the block around it. Measured: with `a()` and `r()` both left as plain
    /// statements, `block_of` resolves both to the *same* block — the one enclosing them —
    /// whose own `start` sits before the region regardless of which fixture it is, so
    /// `on_all_paths_within` cannot tell "release inside" from "release after" apart at all;
    /// the `switch` gives `release` a block whose `start` genuinely differs between the two
    /// fixtures, which is the fact the function is supposed to be reading.
    fn block_scope_fixture<'t>(
        tree: &'t tree_sitter::Tree,
        source: &str,
        cfg: &Cfg<'t>,
    ) -> (BlockId, std::ops::Range<usize>, BlockId) {
        let a_call = find_all(tree, "call_expression")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "a()")
            .unwrap();
        let r_call = find_all(tree, "call_expression")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "r()")
            .unwrap();
        let region = super::super::cfg_build::enclosing_block(a_call)
            .unwrap()
            .byte_range();
        let acquire = cfg.block_of(a_call).unwrap();
        let release = cfg.block_of(r_call).unwrap();
        (acquire, region, release)
    }

    #[test]
    fn a_release_inside_the_block_discharges_block_scope() {
        // acquire and release both inside the outer block; leaving it (at `after`), the
        // release has already run — on the switch's one path, `r()` before `after()`.
        let source = "function f() { { a(); switch (x) { default: r(); } } after(); }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let (acquire, region, release) = block_scope_fixture(&tree, source, &cfg);
        assert!(cfg.on_all_paths_within(acquire, region, &[release]));
    }

    #[test]
    fn a_release_after_the_block_does_not_discharge_block_scope() {
        // release is outside the (smaller) block; leaving it, `r()` has not run yet.
        let source = "function f() { { a(); } switch (x) { default: r(); } }";
        let tree = parse(source);
        let cfg = build(&tree, source);
        let (acquire, region, release) = block_scope_fixture(&tree, source, &cfg);
        assert!(!cfg.on_all_paths_within(acquire, region, &[release]));
    }
}
