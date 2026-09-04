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
/// `True` and `False` name the outcome of the source block's terminating condition. For
/// `a ?? b` and `a?.b` the condition is "the left operand is non-nullish", so `False` is
/// the edge that continues to the right-hand side. That is a stated convention rather
/// than a derivation, because two readers would otherwise pick opposite labels.
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

    /// Attribute `node` to `id`.
    pub(crate) fn attribute(&mut self, id: BlockId, node: Node<'t>) {
        self.blocks[id.0].nodes.push(node);
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
    /// Among every attribution whose byte range contains `node`'s first byte, the
    /// **narrowest** wins; ties break to the lowest [`BlockId`]. Innermost-wins is what
    /// makes this correct given that a split statement is attributed to its own join
    /// block: a plain containment lookup would answer with the enclosing statement's
    /// block for every fragment of it.
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
