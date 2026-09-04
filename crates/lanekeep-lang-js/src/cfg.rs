//! The control-flow graph: blocks, edges, and the source-order guarantee.
//!
//! A [`Cfg`] is per-function and per-parse. It is built by `cfg_build` and queried by
//! `cfg_query`; this file owns the shape and the one invariant both of those rest on —
//! that [`BlockId`] order **is** source order.
//!
//! Blocks unreachable from [`Cfg::entry`] are kept rather than pruned. Dead code is
//! information a consumer may want, and pruning would discard it silently.

use std::ops::Range;

use tree_sitter::Node;

/// A block's identity within one [`Cfg`], and its position in source order.
///
/// **Zero is a valid identity.** [`Cfg::entry`] is `BlockId(0)`, so nothing may test a
/// block for presence by comparing against zero or by truthiness; absence is
/// `Option<BlockId>`. AGENTS.md records the same trap costing `no-unwrap` its whole
/// `#[test]` exemption, silently, because the discarded check only ever removed
/// violations.
///
/// A `usize` rather than a `u32`: a narrower id would need a fallible conversion at every
/// allocation, and the workspace denies the `unwrap` that would make it infallible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub(crate) usize);

impl BlockId {
    /// The index this id addresses.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// What decided that control took this edge.
///
/// `True` and `False` name the outcome of the source block's terminating condition. That
/// is a stated convention rather than a derivation, because two readers would otherwise
/// pick opposite labels.
///
/// `&&` takes `True` to its right operand; `||` and `??` take `False` to theirs. `a ?? b`
/// and `a?.b` share one condition — "the left operand is non-nullish" — and land on
/// *opposite* edges, which is the half that has to be said rather than inferred: `??`
/// evaluates `b` when the condition fails, so `False` goes there, while `?.` continues
/// the chain when it holds, so `True` goes to the rest of the chain and `False` skips
/// straight to the join, where the whole chain is `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Straight-line flow, or the only way out of the block.
    Normal,
    /// The condition held.
    True,
    /// The condition did not hold.
    False,
    /// A `throw` reaching its handler, or leaving the function.
    Exception,
}

/// One edge out of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// Where control goes.
    pub target: BlockId,
    /// What decided it.
    pub kind: EdgeKind,
    /// A loop back-edge, or a `continue`.
    ///
    /// Informational. Traversal terminates by its visited set and never by reading this,
    /// so a wrong value here cannot hang a walk — it can only mislead a reader.
    pub back: bool,
}

/// A straight-line run of nodes with one entry and one exit.
#[derive(Debug)]
pub struct Block<'t> {
    /// The AST nodes attributed to this block, in source order.
    ///
    /// A statement with no internal branch is attributed whole. A statement whose
    /// expressions branch is split across fragment blocks, each attributed the operand it
    /// evaluates, with the statement node itself attributed to the fragment where
    /// evaluation completes.
    pub nodes: Vec<Node<'t>>,
    /// Edges out, in the order construction added them.
    pub successors: Vec<Edge>,
    /// Blocks with an edge into this one, ascending. Derived by [`Cfg::finish`].
    pub predecessors: Vec<BlockId>,
    /// The first byte this block covers, and its ordering key.
    pub start: usize,
}

/// A per-function control-flow graph.
#[derive(Debug)]
pub struct Cfg<'t> {
    pub(crate) blocks: Vec<Block<'t>>,
    pub(crate) entry: BlockId,
    pub(crate) exit: BlockId,
    pub(crate) root: Range<usize>,
}

impl<'t> Cfg<'t> {
    /// A graph over `root` holding only its synthetic entry and exit.
    ///
    /// Entry is allocated first and exit second, which is load-bearing rather than
    /// incidental: [`Self::finish`] sorts on `start` with a stable sort, so a tie —
    /// a `program` whose first statement begins at byte 0 — resolves by allocation
    /// order. Allocating them in any other order would silently move them.
    pub(crate) fn new_empty(root: Range<usize>) -> Self {
        let mut cfg = Self {
            blocks: Vec::new(),
            entry: BlockId(0),
            exit: BlockId(1),
            root: root.clone(),
        };
        cfg.entry = cfg.alloc(root.start);
        cfg.exit = cfg.alloc(root.end);
        cfg
    }

    /// A fresh empty block covering from `start`.
    pub(crate) fn alloc(&mut self, start: usize) -> BlockId {
        self.blocks.push(Block {
            nodes: Vec::new(),
            successors: Vec::new(),
            predecessors: Vec::new(),
            start,
        });
        BlockId(self.blocks.len() - 1)
    }

    /// Add an edge, ignoring an exact duplicate.
    pub(crate) fn edge(&mut self, from: BlockId, to: BlockId, kind: EdgeKind, back: bool) {
        let edge = Edge {
            target: to,
            kind,
            back,
        };
        let successors = &mut self.blocks[from.0].successors;
        if !successors.contains(&edge) {
            successors.push(edge);
        }
    }

    /// Attribute `node` to `id`, ignoring an exact duplicate.
    ///
    /// Symmetric with [`Self::edge`] directly above, and for the same reason. One
    /// node reaching one block twice is not a caller bug: `cfg_build`'s chain walk
    /// attributes a chain to its join, and an enclosing `&&`/`||`/`??` attributes that
    /// same chain, as its left operand, to the block the operand's evaluation returned —
    /// which is that very join. [`Block::nodes`] is "the AST nodes attributed to this
    /// block", so a consumer iterating it has to see each of them once.
    pub(crate) fn attribute(&mut self, id: BlockId, node: Node<'t>) {
        let nodes = &mut self.blocks[id.0].nodes;
        if !nodes.iter().any(|seen| seen.id() == node.id()) {
            nodes.push(node);
        }
    }

    /// Renumber every block into source order, then derive the predecessor lists.
    ///
    /// Called once, after construction. Sorting here rather than at each call site is
    /// what makes determinism a property of the type: a consumer iterating `0..n` gets
    /// source order without having to remember to ask for it.
    pub(crate) fn finish(&mut self) {
        let mut order: Vec<usize> = (0..self.blocks.len()).collect();
        // Allocation order is part of the sort key, not an inherited property of a stable
        // sort: `sort_unstable_by_key` would be tempting and is not equivalent for these
        // sizes. `Vec::sort_by_key` is stable, and pdqsort's own small-slice fallback
        // (insertion sort, under 21 elements) happens to preserve ties too, so a fixture
        // this small cannot distinguish the two — the tie-break has to be data, not an
        // implementation accident of whichever sort is called.
        order.sort_by_key(|&index| (self.blocks[index].start, index));

        let mut new_of = vec![0usize; self.blocks.len()];
        for (new, &old) in order.iter().enumerate() {
            new_of[old] = new;
        }

        // `order` is a permutation of the indices, so every slot is taken exactly once.
        let mut taken: Vec<Option<Block<'t>>> = self.blocks.drain(..).map(Some).collect();
        let mut blocks: Vec<Block<'t>> = Vec::with_capacity(taken.len());
        for &old in &order {
            if let Some(block) = taken[old].take() {
                blocks.push(block);
            }
        }

        for block in &mut blocks {
            block.predecessors.clear();
            for edge in &mut block.successors {
                edge.target = BlockId(new_of[edge.target.0]);
            }
        }

        // Predecessors are derived from the remapped successors rather than remapped
        // themselves. A second remapping could disagree with the edges; a projection
        // cannot.
        for from in 0..blocks.len() {
            let targets: Vec<BlockId> = blocks[from]
                .successors
                .iter()
                .map(|edge| edge.target)
                .collect();
            for target in targets {
                let predecessors = &mut blocks[target.0].predecessors;
                if !predecessors.contains(&BlockId(from)) {
                    predecessors.push(BlockId(from));
                }
            }
        }

        self.entry = BlockId(new_of[self.entry.0]);
        self.exit = BlockId(new_of[self.exit.0]);
        self.blocks = blocks;
    }

    /// The synthetic block every path starts from.
    #[must_use]
    pub fn entry(&self) -> BlockId {
        self.entry
    }

    /// The synthetic block every completing path ends at.
    #[must_use]
    pub fn exit(&self) -> BlockId {
        self.exit
    }

    /// One block by id.
    #[must_use]
    pub fn block(&self, id: BlockId) -> &Block<'t> {
        &self.blocks[id.0]
    }

    /// Every block, in source position order.
    #[must_use = "blocks() has no side effect; discarding its result visits nothing"]
    pub fn blocks(&self) -> impl Iterator<Item = (BlockId, &Block<'t>)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (BlockId(index), block))
    }

    /// The block `node` belongs to, or `None` when nothing attributed covers it.
    ///
    /// Two stages, and their order is the whole point.
    ///
    /// **Exact identity first.** If some block was attributed this very node, that block
    /// is the answer. An exact match is the most precise attribution there can be, so
    /// preferring it can never make an answer worse — and it is what makes "where does
    /// this expression complete?" answerable for a construct that branches. `cfg_build`
    /// attributes an optional chain to its join, the merge point both the short-circuited
    /// and the completed path reach; containment alone would answer with the block holding
    /// the chain's *base*, which sits on one branch of it, because a chain starts where its
    /// base starts and the base's own attribution is narrower.
    ///
    /// **Containment second**, for a node nothing attributed directly — a fragment of some
    /// larger attributed construct. Among every attribution whose byte range contains
    /// `node`'s first byte, the **narrowest** wins. Innermost-wins is what makes this
    /// correct given that a split statement is attributed to its own join block: a plain
    /// containment lookup would answer with the enclosing statement's block for every
    /// fragment of it.
    ///
    /// Ties break to the lowest [`BlockId`] in both stages.
    ///
    /// **One node can belong to several blocks.** A `finally` body is emitted once per
    /// distinct continuation, so every node inside one is attributed to each copy — and
    /// the copies are real, separate blocks, which is what keeps "the finalizer is on
    /// every path out of the `try`" a fact about the edge set. Exact identity then matches
    /// more than one block and this answers with the lowest-numbered of them: one true
    /// answer among several, never a merge. A consumer that needs a property to hold of
    /// *every* copy has to scan [`Self::blocks`] rather than ask here.
    ///
    /// The `root`-containment check below is a correctness gate: it is what keeps a
    /// node attributed from outside `root` from answering a query for an offset outside
    /// it.
    #[must_use]
    pub fn block_of(&self, node: Node<'_>) -> Option<BlockId> {
        let offset = node.start_byte();
        if !self.root.contains(&offset) {
            return None;
        }
        // Blocks are visited in ascending index order, so the first hit is the lowest id.
        for (index, block) in self.blocks.iter().enumerate() {
            if block.nodes.iter().any(|seen| seen.id() == node.id()) {
                return Some(BlockId(index));
            }
        }
        let mut best: Option<(usize, BlockId)> = None;
        for (index, block) in self.blocks.iter().enumerate() {
            for attributed in &block.nodes {
                let range = attributed.byte_range();
                if !range.contains(&offset) {
                    continue;
                }
                let width = range.end - range.start;
                if best.is_none_or(|(seen, _)| width < seen) {
                    best = Some((width, BlockId(index)));
                }
            }
        }
        best.map(|(_, id)| id)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use lanekeep_lang::Language;
    use tree_sitter::{Node, Parser, Tree};

    use crate::{Tsx, TypeScript};

    /// Parse with the TypeScript grammar, refusing a fixture that does not parse.
    pub(crate) fn parse(source: &str) -> Tree {
        parse_with(&TypeScript, source)
    }

    /// Parse with the TSX grammar.
    pub(crate) fn parse_tsx(source: &str) -> Tree {
        parse_with(&Tsx, source)
    }

    fn parse_with(language: &dyn Language, source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&language.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parser returns a tree");
        // A sample that silently fails to parse is how AGENTS.md's carrier count came
        // back 9 instead of 18. Refuse one here rather than asserting against a
        // half-parsed tree.
        assert!(
            !tree.root_node().has_error(),
            "fixture does not parse:\n{source}"
        );
        tree
    }

    /// Every node of `kind`, in source order.
    pub(crate) fn find_all<'t>(tree: &'t Tree, kind: &str) -> Vec<Node<'t>> {
        let mut found = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                found.push(node);
            }
            for index in (0..node.child_count()).rev() {
                // `child_count` answers in `usize`; `child` takes `u32`. tree-sitter's own
                // node representation is `u32`-indexed, so this cannot truncate a real tree.
                let index = u32::try_from(index).expect("tree-sitter child index fits in u32");
                if let Some(child) = node.child(index) {
                    stack.push(child);
                }
            }
        }
        found.sort_by_key(Node::start_byte);
        found
    }

    /// The first node of `kind` in source order.
    pub(crate) fn find<'t>(tree: &'t Tree, kind: &str) -> Node<'t> {
        let mut all = find_all(tree, kind);
        assert!(!all.is_empty(), "no `{kind}` in the fixture");
        all.remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockId, Cfg, EdgeKind};

    /// A three-block graph allocated out of source order, so `finish` has something to do.
    fn out_of_order() -> Cfg<'static> {
        let mut cfg = Cfg::new_empty(0..100);
        let entry = cfg.entry();
        let exit = cfg.exit();
        let late = cfg.alloc(60);
        let early = cfg.alloc(10);
        cfg.edge(entry, late, EdgeKind::Normal, false);
        cfg.edge(late, early, EdgeKind::Normal, true);
        cfg.edge(early, exit, EdgeKind::Normal, false);
        cfg.finish();
        cfg
    }

    #[test]
    fn block_ids_are_source_order_after_finish() {
        let cfg = out_of_order();
        let starts: Vec<usize> = cfg.blocks().map(|(_, block)| block.start).collect();
        assert_eq!(starts, vec![0, 10, 60, 100]);
        assert!(
            starts.windows(2).all(|w| w[0] <= w[1]),
            "block ids must be source order, got {starts:?}",
        );
    }

    #[test]
    fn entry_is_first_and_exit_is_last() {
        let cfg = out_of_order();
        assert_eq!(cfg.entry(), BlockId(0));
        assert_eq!(cfg.exit(), BlockId(3));
    }

    #[test]
    fn a_tie_on_start_resolves_to_allocation_order() {
        // A `program` whose first statement begins at byte 0 ties with the synthetic entry.
        // Entry is allocated first, so it must still sort first.
        let mut cfg = Cfg::new_empty(0..10);
        let entry = cfg.entry();
        let first = cfg.alloc(0);
        let exit = cfg.exit();
        cfg.edge(entry, first, EdgeKind::Normal, false);
        cfg.edge(first, exit, EdgeKind::Normal, false);
        cfg.finish();
        assert_eq!(cfg.entry(), BlockId(0));
        assert_eq!(cfg.block(BlockId(1)).start, 0);
    }

    #[test]
    fn finish_remaps_every_edge_target() {
        let cfg = out_of_order();
        // Not entry's own edge: the block at 60 keeps id 2 after renumbering by
        // coincidence of these particular `start` values, so entry(0) -> 60's edge stays
        // `BlockId(2)` whether or not the remap runs, and cannot tell the two cases apart.
        // The block at 60's edge to the block at 10 does move (allocation index 3 -> id
        // 1), which is what actually exercises the remap: skipping it would leave the
        // stale allocation id, 3 — and after renumbering, id 3 is the exit.
        let targets: Vec<BlockId> = cfg
            .block(BlockId(2))
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets, vec![BlockId(1)]);
    }

    #[test]
    fn finish_derives_predecessors_from_the_remapped_edges() {
        let cfg = out_of_order();
        // The block at 10 renumbers to id 1, and its only predecessor is the block at 60.
        assert_eq!(cfg.block(BlockId(1)).predecessors, vec![BlockId(2)]);
        assert_eq!(cfg.block(cfg.entry()).predecessors, Vec::<BlockId>::new());
    }

    #[test]
    fn a_block_with_two_predecessors_lists_them_ascending() {
        // Nothing else in this file builds a block with more than one predecessor, so
        // `Block::predecessors`'s documented "ascending" order has never been exercised.
        let mut cfg = Cfg::new_empty(0..100);
        let entry = cfg.entry();
        let exit = cfg.exit();
        let a = cfg.alloc(10);
        let b = cfg.alloc(20);
        let join = cfg.alloc(30);
        cfg.edge(entry, a, EdgeKind::Normal, false);
        cfg.edge(entry, b, EdgeKind::Normal, false);
        // Added in the order b-then-a, the reverse of their eventual ascending ids, so
        // this cannot pass merely because predecessors happen to record insertion order.
        cfg.edge(b, join, EdgeKind::Normal, false);
        cfg.edge(a, join, EdgeKind::Normal, false);
        cfg.edge(join, exit, EdgeKind::Normal, false);
        cfg.finish();

        // a(10) renumbers to id 1, b(20) to id 2, join(30) to id 3.
        assert_eq!(
            cfg.block(BlockId(3)).predecessors,
            vec![BlockId(1), BlockId(2)]
        );
    }

    #[test]
    fn attributing_one_node_twice_to_one_block_records_it_once() {
        // Not a caller bug: `cfg_build` attributes an optional chain to its join, and an
        // enclosing `??` attributes the same chain, as its left operand, to that same
        // block. `nodes` is what a consumer iterates, so it has to hold each node once.
        let source = "a();";
        let tree = super::testing::parse(source);
        let call = super::testing::find(&tree, "call_expression");
        let mut cfg = Cfg::new_empty(0..source.len());
        let first = cfg.alloc(0);
        let second = cfg.alloc(1);
        cfg.attribute(first, call);
        cfg.attribute(first, call);
        assert_eq!(cfg.block(first).nodes.len(), 1, "one block, one entry");

        // Per block, like `edge`'s guard — not a global "attribute this node once" rule,
        // which would silently drop the second block's claim on it.
        cfg.attribute(second, call);
        assert_eq!(
            cfg.block(second).nodes.len(),
            1,
            "a different block still records it"
        );
    }

    #[test]
    fn block_of_prefers_an_exact_match_to_a_narrower_container() {
        // The two stages, with containment pointing the other way. `a.b` and `a` start at
        // the same byte and `a` is narrower, so containment alone answers with `a`'s block
        // — which is the wrong answer when `a.b` is itself attributed somewhere, because an
        // exact match is the most precise attribution there can be. In `cfg_build` this is
        // an optional chain and its base: the chain completes at its join, the base sits on
        // one branch of it.
        let source = "a.b;";
        let tree = super::testing::parse(source);
        let member = super::testing::find(&tree, "member_expression");
        let object = member
            .child_by_field_name("object")
            .expect("`a.b` has an object");
        let property = member
            .child_by_field_name("property")
            .expect("`a.b` has a property");
        let mut cfg = Cfg::new_empty(0..source.len());
        let narrow = cfg.alloc(0);
        let wide = cfg.alloc(1);
        cfg.attribute(narrow, object);
        cfg.attribute(wide, member);

        assert_eq!(
            cfg.block_of(member),
            Some(wide),
            "an exact match outranks a narrower attribution over the same offset",
        );
        assert_eq!(
            cfg.block_of(object),
            Some(narrow),
            "and so does the base's own"
        );
        // `b` is attributed nowhere, so the containment fallback answers for it.
        assert_eq!(
            cfg.block_of(property),
            Some(wide),
            "a node nothing attributed falls back to narrowest containment",
        );
    }

    #[test]
    fn the_back_flag_survives_renumbering() {
        let cfg = out_of_order();
        let back: Vec<bool> = cfg
            .block(BlockId(2))
            .successors
            .iter()
            .map(|e| e.back)
            .collect();
        assert_eq!(back, vec![true]);
    }

    #[test]
    fn block_of_finds_the_innermost_attribution() {
        let tree = super::testing::parse("function f() { const a = 1; }");
        let statement = super::testing::find(&tree, "lexical_declaration");
        // Not `find(&tree, "identifier")`: the *first* identifier in source order is `f`,
        // the function's own name, whose range sits entirely outside `statement` and so
        // never nests inside it. Picking that one would leave the two attributed ranges
        // disjoint, and `assert_ne!` below would pass for the wrong reason — two unrelated
        // matches, not narrowest-wins. Filter to the identifier actually inside the
        // declaration instead.
        let name = super::testing::find_all(&tree, "identifier")
            .into_iter()
            .find(|node| statement.byte_range().contains(&node.start_byte()))
            .expect("an identifier inside the declaration");

        let mut cfg = Cfg::new_empty(tree.root_node().byte_range());
        let outer = cfg.alloc(statement.start_byte());
        let inner = cfg.alloc(name.start_byte());
        cfg.attribute(outer, statement);
        cfg.attribute(inner, name);
        cfg.finish();

        // The identifier's range is strictly inside the declaration's, so the narrower
        // attribution wins even though both contain the offset.
        let chosen = cfg.block_of(name).expect("inside the root");
        assert_ne!(
            chosen,
            cfg.block_of(statement).expect("inside the root"),
            "the narrower attribution must win, not the enclosing statement's",
        );
        assert_eq!(cfg.block(chosen).nodes.len(), 1);
        assert_eq!(cfg.block(chosen).nodes[0].kind(), "identifier");
    }

    #[test]
    fn block_of_refuses_a_node_outside_the_root() {
        let tree = super::testing::parse("const a = 1;\nfunction f() {}");
        let function = super::testing::find(&tree, "function_declaration");
        let outside = super::testing::find(&tree, "lexical_declaration");
        let mut cfg = Cfg::new_empty(function.byte_range());
        // Load-bearing. With nothing attributed, every block's `nodes` is empty and the
        // containment loop below has nothing to match regardless of the guard, so this
        // test would pass whether or not the guard runs. Attributing `outside` gives the
        // loop a range that trivially contains its own start byte — the guard is then
        // the only thing standing between that and a wrong `Some`.
        cfg.attribute(cfg.entry(), outside);
        cfg.finish();
        assert_eq!(cfg.block_of(outside), None);
    }

    #[test]
    fn an_unattributed_block_is_not_a_match() {
        let tree = super::testing::parse("function f() {}");
        let function = super::testing::find(&tree, "function_declaration");
        let mut cfg = Cfg::new_empty(function.byte_range());
        cfg.finish();
        assert_eq!(cfg.block_of(function), None);
    }

    #[test]
    fn the_test_parser_rejects_a_fixture_that_does_not_parse() {
        // The guard AGENTS.md's miscount lesson asks for, asserted rather than assumed.
        let result = std::panic::catch_unwind(|| super::testing::parse("function ("));
        assert!(
            result.is_err(),
            "a fixture with a syntax error must fail loudly"
        );
    }
}
