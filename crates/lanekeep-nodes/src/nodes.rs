//! Node handles: how a parsed file's nodes cross into a rule.
//!
//! Architecture §14 requires nodes to cross the boundary as opaque handles rather than as
//! materialized objects. Materializing an AST is the cost that makes native tooling with
//! JavaScript plugins slow, and it is a decision that cannot be walked back once rules
//! depend on the object shape. Nothing about that argument is specific to one engine, which
//! is why this type lives in its own crate rather than inside the engine that first needed
//! it — see the crate-level docs for which engines that is.
//!
//! # Why paths rather than stored nodes
//!
//! The obvious arena holds `Node<'tree>` and hands out indices. It does not compile against
//! `lanekeep-js`'s engine: rquickjs's `Function::new` requires `'static` closures, so
//! nothing captured by a host function may borrow from a tree living on the caller's stack.
//!
//! So the arena **owns** the tree and stores, for each handle, the path of child indices
//! from the root. Resolving a handle walks that path — `O(depth)`, where depth is typically
//! ten to thirty — and every method does its work internally rather than returning a
//! borrowed `Node`, which is what keeps the borrow checker satisfied without `unsafe`.
//!
//! Two properties this buys, both of which matter more than the lookup cost, and neither of
//! which is specific to the engine that first needed them:
//!
//! **Laziness.** Only nodes actually handed to a rule are interned. A file whose query
//! matches nothing costs nothing here, which is the whole point of the query gate.
//!
//! **Stable identity.** Handles are interned by node id, so the same node reached twice —
//! a parent lookup from two siblings, say — yields the same number. A rule comparing two
//! handles for equality relies on that, and it has to mean what it appears to mean.

use lanekeep_query::CompiledQuery;
use std::collections::HashMap;

use lanekeep_lang::binding::{Binding, BindingResolver};
use tree_sitter::{Node, Tree};

/// An opaque reference to a node, as seen from rule code.
pub type Handle = u32;

/// A subtree's structural fingerprint: what the fold covered, and how much of it.
///
/// Computed host-side in one walk, so a rule does not pay a per-node boundary crossing to
/// inspect a tree's shape — the exact cost invariant 3 (`docs/architecture.md` §4) exists
/// to prevent. See [`NodeArena::structure_fingerprint`] for the normalization contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureFingerprint {
    /// blake3 of the normalized fold, lowercase hex.
    pub hash: String,
    /// How many nodes the fold covered. Exact, not approximate: it is the thresholding
    /// input, so a rule knows whether a match is trivially small without a second
    /// traversal.
    pub nodes: u32,
}

/// Child indices of a node, as `u32`.
///
/// tree-sitter reports `child_count` as `usize` but takes `u32` in `child`, so the
/// conversion lives here once rather than at each of the three call sites.
fn child_indices(node: Node<'_>) -> std::ops::Range<u32> {
    0..u32::try_from(node.child_count()).unwrap_or(u32::MAX)
}

/// Owns a parsed tree and the handles issued against it.
#[derive(Debug)]
pub struct NodeArena {
    tree: Tree,
    source: String,
    /// Handle to the path of child indices from the root. The root's path is empty.
    paths: Vec<Vec<u32>>,
    /// Node id to handle, so a node reached twice gets the same handle both times.
    by_id: HashMap<usize, Handle>,
}

impl NodeArena {
    /// Take ownership of a tree and its source.
    ///
    /// The root is interned as handle `0`, so rule code always has somewhere to start.
    #[must_use]
    pub fn new(tree: Tree, source: String) -> Self {
        let root_id = tree.root_node().id();
        let mut arena = Self {
            tree,
            source,
            paths: Vec::new(),
            by_id: HashMap::new(),
        };
        arena.paths.push(Vec::new());
        arena.by_id.insert(root_id, 0);
        arena
    }

    /// The root node's handle.
    ///
    /// A constant rather than a method: the root is interned first, so it is always zero.
    pub const ROOT: Handle = 0;

    /// The source this tree was parsed from.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// How many handles have been issued. Interning is lazy, so this reflects how much of
    /// the tree a rule actually touched.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether only the root has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.len() <= 1
    }

    /// Walk a path from the root.
    fn node_at(&self, path: &[u32]) -> Option<Node<'_>> {
        let mut node = self.tree.root_node();
        for index in path {
            node = node.child(*index)?;
        }
        Some(node)
    }

    /// Resolve a handle to its node.
    fn node(&self, handle: Handle) -> Option<Node<'_>> {
        let path = self.paths.get(handle as usize)?;
        self.node_at(path)
    }

    /// Issue a handle for a path, reusing the existing one when the node is already known.
    fn intern(&mut self, id: usize, path: Vec<u32>) -> Handle {
        if let Some(existing) = self.by_id.get(&id) {
            return *existing;
        }
        // A tree large enough to overflow this would have exhausted memory long before.
        let handle = Handle::try_from(self.paths.len()).unwrap_or(Handle::MAX);
        self.paths.push(path);
        self.by_id.insert(id, handle);
        handle
    }

    /// Intern a node reached by extending a known path.
    ///
    /// Split into "read the node's id, then intern" so the immutable borrow of `self` ends
    /// before the mutable one begins.
    fn intern_child(&mut self, parent_path: &[u32], index: u32) -> Option<Handle> {
        let mut path = parent_path.to_vec();
        path.push(index);
        let id = self.node_at(&path)?.id();
        Some(self.intern(id, path))
    }

    /// The node's kind, as the grammar names it.
    #[must_use]
    pub fn kind(&self, handle: Handle) -> Option<&'static str> {
        self.node(handle).map(|node| node.kind())
    }

    /// Whether the node is named, as opposed to an anonymous token such as a bracket.
    #[must_use]
    pub fn is_named(&self, handle: Handle) -> Option<bool> {
        self.node(handle).map(|node| node.is_named())
    }

    /// The source text the node spans.
    #[must_use]
    pub fn text(&self, handle: Handle) -> Option<&str> {
        let node = self.node(handle)?;
        self.source.get(node.byte_range())
    }

    /// One-based line and column of the node's start.
    #[must_use]
    pub fn position(&self, handle: Handle) -> Option<(u32, u32)> {
        let node = self.node(handle)?;
        let start = node.start_position();
        Some((
            u32::try_from(start.row)
                .unwrap_or(u32::MAX)
                .saturating_add(1),
            u32::try_from(start.column)
                .unwrap_or(u32::MAX)
                .saturating_add(1),
        ))
    }

    /// The node's byte range in the source.
    #[must_use]
    pub fn byte_range(&self, handle: Handle) -> Option<(usize, usize)> {
        self.node(handle)
            .map(|node| (node.start_byte(), node.end_byte()))
    }

    /// What the identifier at a handle refers to.
    ///
    /// Takes the resolver rather than holding one so the arena stays language-agnostic,
    /// and does the work internally so the borrowed `Node` never escapes.
    #[must_use]
    pub fn resolve_binding(
        &self,
        handle: Handle,
        resolver: &dyn BindingResolver,
    ) -> Option<Binding> {
        let node = self.node(handle)?;
        resolver.resolve(&self.tree, &self.source, node)
    }

    /// Whether the identifier at a handle shadows an outer binding of the same name.
    #[must_use]
    pub fn is_shadowed(&self, handle: Handle, resolver: &dyn BindingResolver) -> bool {
        self.node(handle)
            .is_some_and(|node| resolver.is_shadowed(&self.tree, &self.source, node))
    }

    /// The tree, for running queries against.
    ///
    /// Nodes obtained this way borrow the arena immutably, so they cannot be interned
    /// while still held. Use [`NodeArena::path_of`] to reduce them to paths first, drop
    /// them, then call [`NodeArena::intern_path`]. The two-phase shape is not incidental:
    /// it is what lets the arena own the tree, which is what makes the handles `'static`
    /// enough for the engine to hold.
    #[must_use]
    pub const fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Reduce a node to a path, so it can be interned after its borrow ends.
    ///
    /// Returns `None` for a node belonging to a different tree. Interning one under a path
    /// that happened to resolve locally would hand a rule a handle pointing at an
    /// unrelated node — wrong results, no error.
    #[must_use]
    pub fn path_of(&self, node: Node<'_>) -> Option<Vec<u32>> {
        // Walk up to the root, recording each child index.
        let mut path = Vec::new();
        let mut current = node;
        while let Some(parent) = current.parent() {
            let index = child_indices(parent)
                .find(|i| parent.child(*i).is_some_and(|c| c.id() == current.id()))?;
            path.push(index);
            current = parent;
        }
        path.reverse();

        if self
            .node_at(&path)
            .is_none_or(|found| found.id() != node.id())
        {
            return None;
        }
        Some(path)
    }

    /// Issue a handle for a path produced by [`NodeArena::path_of`].
    pub fn intern_path(&mut self, path: Vec<u32>) -> Option<Handle> {
        let id = self.node_at(&path)?.id();
        Some(self.intern(id, path))
    }

    /// The node's parent, if it is not the root.
    pub fn parent(&mut self, handle: Handle) -> Option<Handle> {
        let path = self.paths.get(handle as usize)?.clone();
        if path.is_empty() {
            return None;
        }

        let parent_path = path[..path.len() - 1].to_vec();
        let id = self.node_at(&parent_path)?.id();
        Some(self.intern(id, parent_path))
    }

    /// Every child, including anonymous tokens.
    pub fn children(&mut self, handle: Handle) -> Vec<Handle> {
        self.children_matching(handle, false)
    }

    /// Named children only, which is what a rule almost always wants.
    pub fn named_children(&mut self, handle: Handle) -> Vec<Handle> {
        self.children_matching(handle, true)
    }

    /// A structural summary of the subtree rooted at `handle`, computed in one host-side
    /// walk.
    ///
    /// The fold that produces it is the whole normalization contract, stated up front:
    ///
    /// - **Every non-extra node's kind name contributes**, named and anonymous — anonymous
    ///   token kinds are the operators, so `a + b` and `a - b` stay distinct. A node is
    ///   excluded if and only if the grammar marked it `extra` (comments): a doc-comment
    ///   difference is not an implementation difference, so comments change neither the
    ///   bytes nor the count.
    /// - **Token text is erased.** An identifier or literal contributes its kind
    ///   (`identifier`, `number`, `string`) and nothing else. The normalization is
    ///   language-agnostic by construction — there is no per-language table of what counts
    ///   as an identifier to maintain.
    /// - **The fold is framed to be unambiguous.** Every kind name is length-prefixed and
    ///   every node writes its fold-child count, so two different shapes cannot
    ///   concatenate into one preimage, and two trees whose preorder kind sequences agree
    ///   but whose nesting differs stay distinct.
    /// - **`ERROR`/`MISSING` nodes fold like any other kind**: a broken parse hashes as
    ///   its broken shape rather than failing.
    ///
    /// The leading byte of the fold is a format version. The fold encoding is host
    /// behavior a rule's verdict can depend on, and it is not itself in the cache key —
    /// so a future change to this contract must bump it **and** `lanekeep_js`'s
    /// `HOST_API_VERSION` together, or cached verdicts computed under the old encoding
    /// stay valid under the old key.
    ///
    /// The hash is blake3 of the fold bytes, lowercase hex. `None` when the handle does
    /// not resolve — nothing rather than a fabricated shape, the same posture `kind`,
    /// `text` and `position` take.
    #[must_use]
    pub fn structure_fingerprint(&self, handle: Handle) -> Option<StructureFingerprint> {
        let node = self.node(handle)?;
        let mut fold = Fold::new();
        fold.node(node);
        let nodes = fold.nodes;
        Some(StructureFingerprint {
            hash: fold.finish(),
            nodes,
        })
    }

    fn children_matching(&mut self, handle: Handle, named_only: bool) -> Vec<Handle> {
        let Some(path) = self.paths.get(handle as usize).cloned() else {
            return Vec::new();
        };

        let indices: Vec<u32> = {
            let Some(node) = self.node_at(&path) else {
                return Vec::new();
            };
            child_indices(node)
                .filter(|i| !named_only || node.child(*i).is_some_and(|c| c.is_named()))
                .collect()
        };

        indices
            .into_iter()
            .filter_map(|i| self.intern_child(&path, i))
            .collect()
    }

    /// Matches of a query, scoped to one node's subtree.
    ///
    /// Two-phase like everything else here: capture paths are collected while the tree is
    /// borrowed, then interned once that borrow has ended. The arena owns the tree, so a
    /// handle cannot be minted while a `Node` derived from it is alive.
    #[must_use]
    pub fn query_subtree(
        &self,
        handle: Handle,
        query: &CompiledQuery,
    ) -> Vec<Vec<(String, Vec<u32>)>> {
        let Some(path) = self.paths.get(handle as usize) else {
            return Vec::new();
        };
        let Some(node) = self.node_at(path) else {
            return Vec::new();
        };

        let mut found = Vec::new();
        query.for_each_match_in(node, self.source.as_bytes(), |m| {
            found.push(
                m.captures
                    .iter()
                    .filter_map(|(name, node)| {
                        self.path_of(*node).map(|path| ((*name).to_owned(), path))
                    })
                    .collect::<Vec<_>>(),
            );
        });
        found
    }

    /// The nearest ancestor a query matches at, with that match's captures.
    ///
    /// "Matches at" rather than "matches within": the query runs rooted at each ancestor in
    /// turn, and a match counts only if it captured that ancestor. Without that, a query
    /// matching anything anywhere inside would make the outermost ancestor the answer every
    /// time, which is never what a rule walking upward wants.
    #[must_use]
    pub fn closest_ancestor_paths(
        &self,
        handle: Handle,
        query: &CompiledQuery,
    ) -> Option<Vec<(String, Vec<u32>)>> {
        let path = self.paths.get(handle as usize)?.clone();

        // Innermost first, so the closest ancestor wins.
        for depth in (0..path.len()).rev() {
            let Some(ancestor) = self.node_at(&path[..depth]) else {
                continue;
            };

            let mut matched: Option<Vec<(String, Vec<u32>)>> = None;
            query.for_each_match_in(ancestor, self.source.as_bytes(), |m| {
                if matched.is_some() || !m.captures.iter().any(|(_, node)| *node == ancestor) {
                    return;
                }
                matched = Some(
                    m.captures
                        .iter()
                        .filter_map(|(name, node)| {
                            self.path_of(*node).map(|p| ((*name).to_owned(), p))
                        })
                        .collect(),
                );
            });

            if matched.is_some() {
                return matched;
            }
        }
        None
    }

    /// Ancestors, innermost first, ending at the root.
    pub fn ancestors(&mut self, handle: Handle) -> Vec<Handle> {
        let Some(path) = self.paths.get(handle as usize).cloned() else {
            return Vec::new();
        };

        let mut out = Vec::with_capacity(path.len());
        for depth in (0..path.len()).rev() {
            let ancestor_path = path[..depth].to_vec();
            let Some(id) = self.node_at(&ancestor_path).map(|n| n.id()) else {
                break;
            };
            out.push(self.intern(id, ancestor_path));
        }
        out
    }
}

/// One pass of the structure fold: the normalized bytes and the node count, together.
///
/// A `blake3::Hasher` plus a counter, named because the two must never be updated apart —
/// a node that contributes bytes but not a count (or the reverse) would let two trees that
/// differ hash alike, which is the exact failure this type exists to make unaskable.
struct Fold {
    hasher: blake3::Hasher,
    nodes: u32,
}

impl Fold {
    fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        // The fold encoding's format version — see
        // `NodeArena::structure_fingerprint`'s doc comment for what changing it entails.
        hasher.update(&[1]);
        Self { hasher, nodes: 0 }
    }

    /// Fold one node and its subtree, preorder. `extra` nodes (comments) are skipped
    /// entirely: they contribute no bytes, no framing and no count.
    fn node(&mut self, node: Node<'_>) {
        if node.is_extra() {
            return;
        }
        self.nodes = self.nodes.saturating_add(1);

        let kind = node.kind();
        let len = u32::try_from(kind.len()).unwrap_or(u32::MAX);
        self.hasher.update(&len.to_le_bytes());
        self.hasher.update(kind.as_bytes());

        // The fold-child count, *excluding* extras, written before the children so the
        // framing is recoverable without a second pass. A comment between two statements
        // must not renumber anything.
        let mut count_cursor = node.walk();
        let mut count: u32 = 0;
        for child in node.children(&mut count_cursor) {
            if !child.is_extra() {
                count = count.saturating_add(1);
            }
        }
        self.hasher.update(&count.to_le_bytes());

        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            self.node(child);
        }
    }

    fn finish(self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;
    use lanekeep_lang_js::TypeScript;

    use super::*;

    fn arena(source: &str) -> NodeArena {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        NodeArena::new(tree, source.to_owned())
    }

    #[test]
    fn the_root_is_always_handle_zero() {
        let arena = arena("const x = 1;");
        assert_eq!(NodeArena::ROOT, 0);
        assert_eq!(arena.kind(0), Some("program"));
    }

    #[test]
    fn resolves_kind_text_and_position() {
        let mut arena = arena("const x = 1;\nconst y = 2;");
        let statements = arena.named_children(NodeArena::ROOT);
        assert_eq!(statements.len(), 2);

        assert_eq!(arena.kind(statements[0]), Some("lexical_declaration"));
        assert_eq!(arena.text(statements[0]), Some("const x = 1;"));
        assert_eq!(arena.position(statements[0]), Some((1, 1)));
        assert_eq!(arena.position(statements[1]), Some((2, 1)));
    }

    #[test]
    fn walks_down_and_back_up() {
        let mut arena = arena("const x = 1;");
        let root = NodeArena::ROOT;
        let declaration = arena.named_children(root)[0];
        let declarator = arena.named_children(declaration)[0];

        assert_eq!(arena.parent(declarator), Some(declaration));
        assert_eq!(arena.parent(declaration), Some(root));
        assert_eq!(arena.parent(root), None, "the root has no parent");
    }

    #[test]
    fn handles_are_stable_for_the_same_node() {
        // Rules compare handles with `===`, so reaching one node by two routes has to
        // produce the same number. Without interning it would produce two, and a rule
        // checking whether two captures are the same node would silently always say no.
        let mut arena = arena("const x = 1;");
        let root = NodeArena::ROOT;
        let declaration = arena.named_children(root)[0];

        let again = arena.named_children(root)[0];
        assert_eq!(
            declaration, again,
            "the same child must intern to the same handle"
        );

        let declarator = arena.named_children(declaration)[0];
        assert_eq!(
            arena.parent(declarator),
            Some(declaration),
            "reaching a node from below must give the handle it already had"
        );
    }

    #[test]
    fn interning_is_lazy() {
        // The property that makes this cheap: a file whose query matched nothing must not
        // have paid to materialize its tree.
        let arena = arena("const a = 1; const b = 2; function c() { return [1,2,3] }");
        assert!(
            arena.is_empty(),
            "only the root should be interned before any traversal"
        );
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn only_touched_nodes_are_interned() {
        let mut arena = arena("const a = 1; const b = 2; const c = 3;");
        let before = arena.len();
        let _ = arena.named_children(NodeArena::ROOT);
        let after = arena.len();

        assert!(after > before);
        assert!(
            after < 20,
            "should intern three statements, not the whole tree: {after}"
        );
    }

    #[test]
    fn named_children_excludes_anonymous_tokens() {
        let mut arena = arena("const x = 1;");
        let declaration = arena.named_children(NodeArena::ROOT)[0];

        let all = arena.children(declaration);
        let named = arena.named_children(declaration);
        assert!(all.len() > named.len(), "`const` and `;` are anonymous");
        assert!(named.iter().all(|h| arena.is_named(*h) == Some(true)));
    }

    #[test]
    fn ancestors_run_innermost_first_and_end_at_the_root() {
        let mut arena = arena("function f() { return 1; }");
        let root = NodeArena::ROOT;
        let function = arena.named_children(root)[0];
        let body = arena
            .named_children(function)
            .last()
            .copied()
            .expect("has a body");
        let statement = arena.named_children(body)[0];

        let ancestors = arena.ancestors(statement);
        assert_eq!(ancestors.first(), Some(&body), "innermost first");
        assert_eq!(ancestors.last(), Some(&root), "ending at the root");
        assert!(ancestors.contains(&function));
    }

    #[test]
    fn the_root_has_no_ancestors() {
        let mut arena = arena("const x = 1;");
        assert!(arena.ancestors(NodeArena::ROOT).is_empty());
    }

    #[test]
    fn an_unknown_handle_yields_nothing_rather_than_panicking() {
        // Rule code is arbitrary and may pass any number at all.
        let mut arena = arena("const x = 1;");
        assert_eq!(arena.kind(9999), None);
        assert_eq!(arena.text(9999), None);
        assert_eq!(arena.position(9999), None);
        assert_eq!(arena.parent(9999), None);
        assert!(arena.children(9999).is_empty());
        assert!(arena.ancestors(9999).is_empty());
    }

    #[test]
    fn interns_a_node_reached_through_the_tree() {
        // The shape real callers use: find nodes against `tree()`, reduce them to paths
        // while the borrow is live, then intern once it has ended.
        let mut arena = arena("const x = 1;");

        let (path, expected_kind) = {
            let target = arena
                .tree()
                .root_node()
                .child(0)
                .and_then(|n| n.child(1))
                .expect("has a declarator");
            (arena.path_of(target).expect("has a path"), target.kind())
        };

        let handle = arena.intern_path(path.clone()).expect("interns");
        assert_eq!(arena.kind(handle), Some(expected_kind));
        assert_eq!(
            arena.intern_path(path),
            Some(handle),
            "interning the same path twice must give the same handle"
        );
    }

    #[test]
    fn rejects_a_node_from_a_different_tree() {
        // Reducing a foreign node to a path that happens to resolve locally would hand a
        // rule a handle pointing at an unrelated node — wrong results with no error.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let other = parser
            .parse("function totallyDifferent() { return 42 }", None)
            .expect("parses");
        let foreign = other.root_node().child(0).expect("has a child");

        let arena = arena("const x = 1;");
        assert_eq!(
            arena.path_of(foreign),
            None,
            "a node from another tree must not be reducible to a path here"
        );
    }

    #[test]
    fn text_is_correct_for_multibyte_source() {
        let mut arena = arena("const emoji = '🎯';\nconst after = 1;");
        let statements = arena.named_children(NodeArena::ROOT);
        assert_eq!(arena.text(statements[0]), Some("const emoji = '🎯';"));
        assert_eq!(
            arena.position(statements[1]),
            Some((2, 1)),
            "a multibyte character must not shift the following line"
        );
    }

    // --- structure fingerprint ----------------------------------------------------------

    fn py_arena(source: &str) -> NodeArena {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&lanekeep_lang_python::Python.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        NodeArena::new(tree, source.to_owned())
    }

    #[test]
    fn fingerprint_erases_identifier_names() {
        // Two functions differing only in their names and their local identifiers must
        // hash identically — that is the whole point of the fold.
        let a = arena("function f() { return a + b }");
        let b = arena("function g() { return c + d }");
        assert_eq!(
            a.structure_fingerprint(NodeArena::ROOT),
            b.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_erases_literal_values_but_not_kinds() {
        // Identifiers and literals contribute their kind and nothing else, so `1` and `2`
        // are the same node kind (`number`) and hash alike, while `1` and `'a'` are
        // different kinds and must not.
        let one = arena("const x = 1;");
        let two = arena("const x = 2;");
        assert_eq!(
            one.structure_fingerprint(NodeArena::ROOT),
            two.structure_fingerprint(NodeArena::ROOT)
        );

        let string = arena("const x = 'a';");
        assert_ne!(
            one.structure_fingerprint(NodeArena::ROOT),
            string.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_is_operator_sensitive() {
        // Anonymous token kinds are included, so `+` and `-` — the same shape, different
        // operators — cannot hash alike.
        let plus = arena("function f() { return a + b }");
        let minus = arena("function f() { return a - b }");
        assert_ne!(
            plus.structure_fingerprint(NodeArena::ROOT),
            minus.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_is_statement_sensitive() {
        let one = arena("function f() { return a + b }");
        let two = arena("function f() { return a + b; a() }");
        assert_ne!(
            one.structure_fingerprint(NodeArena::ROOT),
            two.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_ignores_comments() {
        // `extra` nodes are excluded from the fold, from the framing and from the count, so
        // a doc-comment difference is not an implementation difference.
        let plain = arena("function f() { return a + b }");
        let commented = arena("// a doc comment\nfunction f() { return a + b }");
        assert_eq!(
            plain.structure_fingerprint(NodeArena::ROOT),
            commented.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_counts_every_non_extra_node_exactly() {
        // `const x = 1;` is program, lexical_declaration, the `const` keyword, the
        // variable_declarator, the `identifier`, the `=`, the `number` and the `;` — eight
        // nodes, anonymous tokens counted. The count is exact and asserted, not
        // approximate: it is the thresholding input, so a rule uses it to skip trivially
        // small matches without a second traversal.
        let one = arena("const x = 1;");
        assert_eq!(
            one.structure_fingerprint(NodeArena::ROOT)
                .expect("the root resolves")
                .nodes,
            8
        );

        let two = arena("const x = 1;\nconst y = 2;");
        assert_eq!(
            two.structure_fingerprint(NodeArena::ROOT)
                .expect("the root resolves")
                .nodes,
            15
        );
    }

    #[test]
    fn fingerprint_is_subtree_scoped() {
        // The fold covers exactly the subtree rooted at the handle. The same expression
        // nested under different parents still hashes identically, which is what a rule
        // fingerprinting a function body relies on.
        let mut in_function = arena("function f() { return a + b }");
        let fn_decl = in_function.named_children(NodeArena::ROOT)[0];
        // `function` keyword, name, parameters, body — the statement block is last.
        let body = in_function.named_children(fn_decl).last().copied().unwrap();
        let return_stmt = in_function.named_children(body)[0];
        let in_function_expr = in_function.named_children(return_stmt)[0];

        let mut in_initializer = arena("const x = a + b;");
        let declaration = in_initializer.named_children(NodeArena::ROOT)[0];
        let declarator = in_initializer.named_children(declaration)[0];
        // name, `=`, value — the expression is last.
        let in_initializer_expr = in_initializer
            .named_children(declarator)
            .last()
            .copied()
            .unwrap();

        assert_eq!(
            in_function.structure_fingerprint(in_function_expr),
            in_initializer.structure_fingerprint(in_initializer_expr)
        );
    }

    #[test]
    fn fingerprint_is_deterministic_across_calls_and_parses() {
        let first = arena("function f() { return a + b }");
        let once = first
            .structure_fingerprint(NodeArena::ROOT)
            .expect("the root resolves");
        let twice = first
            .structure_fingerprint(NodeArena::ROOT)
            .expect("the root resolves");
        assert_eq!(
            once, twice,
            "the same arena must answer the same fingerprint twice"
        );

        let again = arena("function f() { return a + b }");
        assert_eq!(
            once,
            again
                .structure_fingerprint(NodeArena::ROOT)
                .expect("the root resolves"),
            "a fresh parse of the same bytes must hash identically"
        );
    }

    #[test]
    fn fingerprint_of_a_dead_handle_is_none() {
        let arena = arena("const x = 1;");
        assert_eq!(arena.structure_fingerprint(9999), None);
    }

    #[test]
    fn fingerprint_is_language_agnostic() {
        // The fold never consults a per-language table — erasure is "kind name and nothing
        // else" by construction — so the same shape in a non-JS grammar hashes alike too.
        let a = py_arena("def f():\n    return a + b\n");
        let b = py_arena("def g():\n    return c + d\n");
        assert_eq!(
            a.structure_fingerprint(NodeArena::ROOT),
            b.structure_fingerprint(NodeArena::ROOT)
        );
    }

    #[test]
    fn fingerprint_normalization_holds_for_python() {
        // Identifier erasure above is the language-agnosticism claim's primary half; the
        // sensitivity halves have to hold in a non-JS grammar too, or the fold would be
        // language-agnostic only where it was convenient. Anonymous operator kinds and
        // literal kinds are the same mechanism in Python as in TypeScript.
        let plus = py_arena("def f():\n    return a + b\n");
        let minus = py_arena("def f():\n    return a - b\n");
        assert_ne!(
            plus.structure_fingerprint(NodeArena::ROOT),
            minus.structure_fingerprint(NodeArena::ROOT),
            "a `+` and a `-` are different anonymous kinds in Python too"
        );

        let one = py_arena("def f():\n    return 1\n");
        let string = py_arena("def f():\n    return 'a'\n");
        assert_ne!(
            one.structure_fingerprint(NodeArena::ROOT),
            string.structure_fingerprint(NodeArena::ROOT),
            "`integer` and `string` are different kinds in Python too"
        );

        let single = py_arena("def f():\n    return a + b\n");
        let two_statements = py_arena("def f():\n    return a + b\n    g()\n");
        assert_ne!(
            single.structure_fingerprint(NodeArena::ROOT),
            two_statements.structure_fingerprint(NodeArena::ROOT),
            "a second statement changes the shape in Python too"
        );
    }

    #[test]
    fn fingerprint_hashes_a_broken_parse_as_its_broken_shape() {
        // `ERROR`/`MISSING` nodes fold like any other kind rather than failing: a broken
        // parse has to produce a defined, stable fingerprint or a rule cannot decide
        // anything about the file. Assert the source really is a broken parse first, so the
        // test cannot quietly pass over a source that parses clean.
        let source = "const x = ;\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        assert!(
            tree.root_node().has_error(),
            "the fixture must really be broken"
        );

        let arena = NodeArena::new(tree, source.to_owned());
        let first = arena
            .structure_fingerprint(NodeArena::ROOT)
            .expect("a broken parse still folds");
        let second = arena
            .structure_fingerprint(NodeArena::ROOT)
            .expect("a broken parse still folds");
        assert_eq!(
            first, second,
            "the fingerprint of a broken parse must be stable across calls"
        );
        assert!(
            first.nodes >= 2,
            "the fold covered the erroring shape, not nothing"
        );
    }
}
