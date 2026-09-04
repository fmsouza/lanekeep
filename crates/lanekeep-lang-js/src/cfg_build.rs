//! Construction: the walk that turns a parsed function into a [`Cfg`].
//!
//! Two mutually recursive operations. `statement` returns `None` when control cannot fall
//! through — after `return`, `throw`, `break` and `continue` — which is what makes
//! unreachable code fall out of the construction rather than needing to be detected.
//! `expression` returns the block where evaluation completes, which differs from the block
//! it started in exactly when the expression branches.

use tree_sitter::Node;

use crate::cfg::{BlockId, Cfg, EdgeKind};

/// Node kinds that own a control-flow graph.
///
/// `program` is here so a rule can govern module top-level code. The body-less TypeScript
/// declaration forms — `function_signature` and friends — are deliberately absent: no
/// body, no flow.
const ROOT_KINDS: &[&str] = &[
    "program",
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
];

/// Kinds that are a separate graph rather than part of this one.
///
/// Identical to [`ROOT_KINDS`] minus `program`, which cannot nest.
const NESTED_FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
];

/// A `break`/`continue` target.
///
/// Unconditional `expect`, not `cfg_attr(not(test), ..)`: unlike Task 1's crate-internal
/// methods, nothing in this file's own test module constructs one either, so the lint
/// would still fire in a test build if this were gated on `not(test)`.
#[expect(
    dead_code,
    reason = "lanekeep#192 task 4 (loops) is the first construct that pushes one; until \
              then nothing constructs a `Target`. Remove once task 4 does."
)]
struct Target<'s> {
    /// The label this target answers to, if any.
    label: Option<&'s str>,
    break_to: BlockId,
    /// `None` for a `switch` or a labeled block, which `continue` passes through.
    continue_to: Option<BlockId>,
    /// `self.finallys.len()` when this target was pushed. Jumping here unwinds
    /// everything pushed since.
    finally_depth: usize,
}

/// A `finally` clause still in scope, and the copies already emitted for it.
#[expect(
    dead_code,
    reason = "lanekeep#192 task 6 (try/finally) is the first construct that pushes one; \
              until then nothing constructs a `Pending`. Remove once task 6 does."
)]
struct Pending<'t> {
    /// The `statement_block` that is the clause's body.
    body: Node<'t>,
    /// Continuation to copy-entry. A list rather than a map: it holds a handful of
    /// entries, and iterating a hash container is what the determinism requirement
    /// forbids.
    memo: Vec<(BlockId, BlockId)>,
}

/// Where a `throw` goes.
#[expect(
    dead_code,
    reason = "lanekeep#192 task 6 (try/catch) is the first construct that pushes one; \
              until then nothing constructs a `Handler`. Remove once task 6 does."
)]
struct Handler {
    to: BlockId,
    /// Finally levels to unwind before reaching `to`.
    finally_depth: usize,
}

struct Builder<'t, 's> {
    cfg: Cfg<'t>,
    /// Read only by `text`, which nothing calls yet either. Same unconditional-`expect`
    /// reasoning as `Target` above.
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 4 (labeled statements) is the first construct that \
                  calls `text`, the only reader. Remove once task 4 calls it."
    )]
    source: &'s str,
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 4 (loops) is the first construct that reads this \
                  stack; populated from this task so later tasks add no struct churn. \
                  Remove once task 4 reads it."
    )]
    targets: Vec<Target<'s>>,
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 6 (try/finally) is the first construct that reads \
                  this stack; populated from this task so later tasks add no struct \
                  churn. Remove once task 6 reads it."
    )]
    finallys: Vec<Pending<'t>>,
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 6 (try/catch) is the first construct that reads this \
                  stack; populated from this task so later tasks add no struct churn. \
                  Remove once task 6 reads it."
    )]
    handlers: Vec<Handler>,
    /// A label waiting for the loop it belongs to (Task 4).
    ///
    /// A field rather than a parameter: it would otherwise thread through `statement`,
    /// which every construct calls and none of the others needs.
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 4 (labeled loops) is the first construct that reads \
                  this field; populated from this task so later tasks add no struct \
                  churn. Remove once task 4 reads it."
    )]
    pending_label: Option<&'s str>,
}

impl<'t> Cfg<'t> {
    /// Build the graph for `root`, or `None` when `root` owns no flow.
    ///
    /// `None` rather than an error, matching `Language::resolver`'s convention in this
    /// crate: a caller gets nothing back instead of a confidently wrong answer.
    ///
    /// Takes no `&Tree`, unlike `BindingResolver::resolve` beside it. That method walks to
    /// the root and needs one; `root` is given here, and it already carries `'t`. A
    /// parameter that is never read reads as an oversight in a published signature.
    #[must_use]
    pub fn build(source: &str, root: Node<'t>) -> Option<Self> {
        if !ROOT_KINDS.contains(&root.kind()) {
            return None;
        }
        let mut builder = Builder {
            cfg: Self::new_empty(root.byte_range()),
            source,
            targets: Vec::new(),
            finallys: Vec::new(),
            handlers: Vec::new(),
            pending_label: None,
        };

        let entry = builder.cfg.entry();
        let start = builder.cfg.alloc(root.start_byte());
        builder.cfg.edge(entry, start, EdgeKind::Normal, false);

        let tail = match root.kind() {
            "program" => builder.statements(root, start),
            _ => match root.child_by_field_name("body") {
                // An arrow with an expression body is a single implicit return.
                Some(body) if body.kind() == "statement_block" => builder.statement(body, start),
                Some(body) => {
                    let end = builder.expression(body, start);
                    builder.cfg.attribute(end, body);
                    Some(end)
                }
                None => Some(start),
            },
        };

        let exit = builder.cfg.exit();
        if let Some(tail) = tail {
            builder.cfg.edge(tail, exit, EdgeKind::Normal, false);
        }

        let mut cfg = builder.cfg;
        cfg.finish();
        Some(cfg)
    }
}

impl<'t, 's> Builder<'t, 's> {
    /// The text of `node`, carrying the *source's* lifetime rather than a borrow of `self`.
    ///
    /// Load-bearing rather than a convenience. `&self.source[node.byte_range()]` written
    /// inline borrows `*self`, so the result cannot then be pushed into `self.targets` —
    /// the borrow checker refuses it, and the diagnostic is about `self` rather than about
    /// the string. Copying the `&'s str` out first decouples the two.
    #[expect(
        dead_code,
        reason = "lanekeep#192 task 4 (labeled statements) is the first construct that \
                  needs a label's text; nothing calls this until then. Remove once task \
                  4 calls it."
    )]
    fn text(&self, node: Node<'_>) -> &'s str {
        let source: &'s str = self.source;
        &source[node.byte_range()]
    }

    /// Walk every named child of `parent` in order.
    fn statements(&mut self, parent: Node<'t>, current: BlockId) -> Option<BlockId> {
        let mut cursor = parent.walk();
        let children: Vec<Node<'t>> = parent.named_children(&mut cursor).collect();
        let mut block = current;
        for child in children {
            block = self.statement(child, block)?;
        }
        Some(block)
    }

    /// Walk one statement. `None` when control cannot fall through it.
    fn statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        match node.kind() {
            "statement_block" => self.statements(node, current),
            "if_statement" => self.if_statement(node, current),
            "empty_statement" => Some(current),
            // Deprecated syntax that still parses. Without this arm its body would fall to
            // the catch-all and be walked as an *expression*, so a `return` inside one
            // would produce no exit edge at all — silently.
            "with_statement" => match node.child_by_field_name("body") {
                Some(body) => self.statement(body, current),
                None => Some(current),
            },
            // A minimal placeholder ahead of Task 6, which owns `return_statement` for real
            // and replaces this arm wholesale once `finallys` unwinding exists. Declared no
            // fields; the operand is `named_child(0)`. Without this arm `return` falls to
            // the catch-all, which always returns `Some` — so nothing in this file could
            // ever produce the "does not fall through" `None` this module's own top-level
            // doc comment promises, and a `return` would never reach `exit` except by the
            // coincidence of being a function's literal last statement.
            "return_statement" => {
                let end = match node.named_child(0) {
                    Some(operand) => self.expression(operand, current),
                    None => current,
                };
                self.cfg.attribute(end, node);
                let exit = self.cfg.exit();
                self.cfg.edge(end, exit, EdgeKind::Normal, false);
                None
            }
            // Tasks 3-6 add arms here (throw, break/continue, loops, switch, try/finally).
            // Everything unlisted is a statement whose only flow is through its own
            // expressions.
            _ => {
                let end = self.expression(node, current);
                self.cfg.attribute(end, node);
                Some(end)
            }
        }
    }

    /// Walk an expression, returning the block where its evaluation completes.
    ///
    /// Task 3 replaces the body with the splitting version. Until then it descends only
    /// far enough to stop at a nested function, which must not be walked.
    fn expression(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        if NESTED_FUNCTION_KINDS.contains(&node.kind()) {
            self.cfg.attribute(current, node);
            return current;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        let mut block = current;
        for child in children {
            block = self.expression(child, block);
        }
        block
    }

    fn if_statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        let condition = node.child_by_field_name("condition")?;
        let test = self.expression(condition, current);
        self.cfg.attribute(test, condition);
        // The statement itself is attributed to the fragment where its evaluation
        // completes, same as the catch-all arm of `statement` — `Block::nodes`'s contract,
        // not special to `if`. Without this, `cfg.block_of(if_statement_node)` answers
        // `None`: the node's own start byte sits before `condition`'s, so nothing
        // attributed here would otherwise contain it.
        self.cfg.attribute(test, node);

        let join = self.cfg.alloc(node.end_byte());
        let mut reachable = false;

        let consequence = node.child_by_field_name("consequence")?;
        let then_entry = self.cfg.alloc(consequence.start_byte());
        self.cfg.edge(test, then_entry, EdgeKind::True, false);
        if let Some(tail) = self.statement(consequence, then_entry) {
            self.cfg.edge(tail, join, EdgeKind::Normal, false);
            reachable = true;
        }

        // `alternative` is an `else_clause`, not a statement: its single named child is
        // the body. Walking the clause itself would send it through the catch-all arm of
        // `statement` and lose every branch inside it.
        if let Some(alternative) = node
            .child_by_field_name("alternative")
            .and_then(|c| c.named_child(0))
        {
            let else_entry = self.cfg.alloc(alternative.start_byte());
            self.cfg.edge(test, else_entry, EdgeKind::False, false);
            if let Some(tail) = self.statement(alternative, else_entry) {
                self.cfg.edge(tail, join, EdgeKind::Normal, false);
                reachable = true;
            }
        } else {
            self.cfg.edge(test, join, EdgeKind::False, false);
            reachable = true;
        }

        reachable.then_some(join)
    }
}

#[cfg(test)]
mod tests {
    use crate::cfg::testing::{find, find_all, parse, parse_tsx};
    use crate::cfg::{BlockId, Cfg, EdgeKind};

    /// Every block reachable from entry, by the kinds of the nodes attributed to it.
    fn shape(cfg: &Cfg<'_>) -> Vec<Vec<&'static str>> {
        cfg.blocks()
            .map(|(_, b)| b.nodes.iter().map(tree_sitter::Node::kind).collect())
            .collect()
    }

    fn successors(cfg: &Cfg<'_>, id: BlockId) -> Vec<(BlockId, EdgeKind)> {
        cfg.block(id)
            .successors
            .iter()
            .map(|e| (e.target, e.kind))
            .collect()
    }

    #[test]
    fn a_function_declaration_is_a_root() {
        let tree = parse("function f() { a(); }");
        let function = find(&tree, "function_declaration");
        let cfg = Cfg::build("function f() { a(); }", function);
        assert!(cfg.is_some());
    }

    #[test]
    fn the_program_is_a_root() {
        let source = "a();";
        let tree = parse(source);
        assert!(Cfg::build(source, tree.root_node()).is_some());
    }

    #[test]
    fn a_node_that_is_not_a_root_is_refused() {
        let source = "a();";
        let tree = parse(source);
        let call = find(&tree, "call_expression");
        assert!(Cfg::build(source, call).is_none());
    }

    #[test]
    fn straight_line_statements_share_one_block() {
        let source = "function f() { a(); b(); c(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let bodies: Vec<Vec<&str>> = shape(&cfg).into_iter().filter(|k| !k.is_empty()).collect();
        assert_eq!(bodies, vec![vec!["expression_statement"; 3]]);
    }

    #[test]
    fn an_if_splits_into_test_then_and_join() {
        let source = "function f() { if (c) { a(); } b(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let test = cfg.block_of(find(&tree, "if_statement")).unwrap();
        let kinds: Vec<EdgeKind> = successors(&cfg, test).into_iter().map(|(_, k)| k).collect();
        assert!(kinds.contains(&EdgeKind::True), "got {kinds:?}");
        assert!(kinds.contains(&EdgeKind::False), "got {kinds:?}");
    }

    #[test]
    fn an_else_clause_is_unwrapped_to_its_statement() {
        // `alternative` is an `else_clause`, not a statement. Walking it directly drops
        // the body, which is exactly what this asserts against.
        let source = "function f() { if (c) { a(); } else { b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let calls = find_all(&tree, "expression_statement");
        assert_eq!(calls.len(), 2);
        for call in calls {
            assert!(
                cfg.block_of(call).is_some(),
                "`{}` reached no block",
                call.kind()
            );
        }
    }

    #[test]
    fn an_else_if_chains_rather_than_nesting_a_clause() {
        let source = "function f() { if (a) { x(); } else if (b) { y(); } else { z(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        for call in find_all(&tree, "expression_statement") {
            assert!(cfg.block_of(call).is_some());
        }
    }

    #[test]
    fn both_arms_returning_leaves_the_join_unreachable() {
        let source = "function f() { if (c) { return 1; } else { return 2; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        // Every block after the `if` has no predecessors: nothing falls through to it.
        let orphans = cfg
            .blocks()
            .filter(|(id, b)| b.predecessors.is_empty() && *id != cfg.entry())
            .count();
        assert!(orphans >= 1, "the join must be left unreachable");
    }

    #[test]
    fn a_nested_function_is_one_opaque_node() {
        let source = "function f() { const g = () => { inner(); }; outer(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let inner = find(&tree, "call_expression");
        // `inner()` is the first call in source order and lives inside the arrow. It must
        // resolve to the block holding the arrow, not to a block of its own.
        let arrow = find(&tree, "arrow_function");
        assert_eq!(cfg.block_of(inner), cfg.block_of(arrow));
        assert!(
            !cfg.blocks().any(
                |(_, b)| b.nodes.iter().any(|n| n.kind() == "statement_block"
                    && n.start_byte() > arrow.start_byte()
                    && n.end_byte() < arrow.end_byte())
            ),
            "the arrow's body must not be walked",
        );
    }

    #[test]
    fn an_arrow_with_an_expression_body_is_a_root() {
        let source = "const f = (x) => x + 1;";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "arrow_function")).unwrap();
        // The body is a single implicit return, so it is attributed and the entry block
        // has a way out. Task 7 adds the reachability form of the same claim.
        assert!(cfg.block_of(find(&tree, "binary_expression")).is_some());
        assert!(!cfg.block(cfg.entry()).successors.is_empty());
    }

    #[test]
    fn a_with_statement_walks_its_body_as_statements() {
        // Deprecated, and it parses, so omitting it drops every statement inside one.
        let source = "function f(o) { with (o) { return 1; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ret = cfg.block_of(find(&tree, "return_statement")).unwrap();
        let targets: Vec<BlockId> = cfg.block(ret).successors.iter().map(|e| e.target).collect();
        assert_eq!(
            targets,
            vec![cfg.exit()],
            "a return inside `with` must reach the exit"
        );
    }

    #[test]
    fn tsx_builds_the_same_graph_as_typescript() {
        let source = "function f() { if (c) { a(); } else { b(); } }";
        let ts = parse(source);
        let tsx = parse_tsx(source);
        let a = Cfg::build(source, find(&ts, "function_declaration")).unwrap();
        let b = Cfg::build(source, find(&tsx, "function_declaration")).unwrap();
        assert_eq!(shape(&a), shape(&b));
        let edges = |c: &Cfg<'_>| -> Vec<Vec<(usize, EdgeKind)>> {
            c.blocks()
                .map(|(_, bl)| {
                    bl.successors
                        .iter()
                        .map(|e| (e.target.index(), e.kind))
                        .collect()
                })
                .collect()
        };
        assert_eq!(edges(&a), edges(&b));
    }
}
