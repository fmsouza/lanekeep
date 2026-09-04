//! Construction: the walk that turns a parsed function into a [`Cfg`].
//!
//! Two operations. `statement` returns `None` when control cannot fall through — after
//! `return`, `throw`, `break` and `continue` — which is what makes unreachable code fall out
//! of the construction rather than needing to be detected. `expression` returns the block
//! where evaluation completes, which differs from the block it started in exactly when the
//! expression branches.
//!
//! `statement` calls `expression`, never the reverse. There is no cycle, because a nested
//! function is attributed whole rather than descended into — so no expression ever needs to
//! reach back into statement territory.

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

/// Postfix kinds that can extend a chain.
///
/// A chain is spelled outside-in by the tree — `a?.b.c` is
/// `member_expression(object: member_expression(a ?. b), property: c)` — and evaluated
/// inside-out. `new_expression` is deliberately absent: its operand is a `constructor`
/// field and `new a?.b()` is not legal syntax, so it never continues a chain.
const POSTFIX_KINDS: &[&str] = &[
    "member_expression",
    "subscript_expression",
    "call_expression",
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
    /// Read only by `text`.
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
            // Deprecated syntax that still parses. Without an arm here its body would
            // fall to the catch-all and be walked as an *expression*, so a `return` inside
            // one would produce no exit edge at all — silently.
            "with_statement" => {
                // The `object` field is required by the grammar and must be evaluated:
                // `with (a && b) { ... }` carries Task 3's short-circuit edges inside it,
                // and delegating straight to the body would drop them silently.
                let mut block = current;
                if let Some(object) = node.child_by_field_name("object") {
                    block = self.expression(object, block);
                }
                self.cfg.attribute(block, node);
                match node.child_by_field_name("body") {
                    Some(body) => self.statement(body, block),
                    None => Some(block),
                }
            }
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

    /// Operators that branch. `in` and `instanceof` are in `binary_expression`'s operator
    /// set and are not among them.
    fn short_circuit_edges(operator: &str) -> Option<(EdgeKind, EdgeKind)> {
        // (kind of the edge to the right operand, kind of the edge to the join)
        match operator {
            "&&" => Some((EdgeKind::True, EdgeKind::False)),
            // `||` and `??` both continue to the right operand when the left did not
            // satisfy the test — falsy for `||`, nullish for `??`.
            "||" | "??" => Some((EdgeKind::False, EdgeKind::True)),
            _ => None,
        }
    }

    /// The sub-expression a postfix node applies to: `object` for a member or subscript
    /// access, `function` for a call.
    fn postfix_base(node: Node<'_>) -> Option<Node<'_>> {
        node.child_by_field_name("object")
            .or_else(|| node.child_by_field_name("function"))
    }

    /// The `?.` that makes this one link short-circuit, if it has one.
    ///
    /// Two spellings, and only the first is what the node-types file advertises.
    /// `member_expression` and `subscript_expression` declare an `optional_chain` field,
    /// whose value is a named node of that kind. `call_expression` declares neither the
    /// field nor a child of that kind: `common/define-grammar.js` overrides the base call
    /// rule so that TypeScript's optional form is
    /// `seq(field('function', ..), '?.', field('type_arguments', ..), field('arguments', ..))`
    /// — a bare *anonymous* `?.` token. Matching only the first spelling is why `f?.()`
    /// and `obj.method?.()` were modelled as unconditional calls.
    ///
    /// Returned rather than reduced to a `bool` because two other things need the node
    /// itself: the continuation block starts where the marker ends, and the marker is
    /// punctuation that must be kept out of `Block::nodes`.
    fn optional_marker(node: Node<'_>) -> Option<Node<'_>> {
        if let Some(field) = node.child_by_field_name("optional_chain") {
            return Some(field);
        }
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|child| child.kind() == "optional_chain" || child.kind() == "?.")
    }

    /// Whether `node` is the outermost link of its postfix chain.
    ///
    /// Only the root builds the chain; every inner link is reached from it and is never
    /// dispatched on its own. A postfix node whose parent is postfix but which is not
    /// that parent's base — the index in `x[a?.b]`, say — is a chain of its own.
    fn is_chain_root(node: Node<'_>) -> bool {
        match node.parent() {
            Some(parent) if POSTFIX_KINDS.contains(&parent.kind()) => {
                Self::postfix_base(parent).map(|base| base.id()) != Some(node.id())
            }
            _ => true,
        }
    }

    /// Whether any link in the chain rooted at `node` short-circuits.
    fn chain_has_optional(node: Node<'_>) -> bool {
        let mut link = node;
        loop {
            if Self::optional_marker(link).is_some() {
                return true;
            }
            match Self::postfix_base(link) {
                Some(base) if POSTFIX_KINDS.contains(&base.kind()) => link = base,
                _ => return false,
            }
        }
    }

    fn expression(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        if NESTED_FUNCTION_KINDS.contains(&node.kind()) {
            self.cfg.attribute(current, node);
            return current;
        }

        match node.kind() {
            "binary_expression" => {
                let operator = node.child_by_field_name("operator").map(|op| self.text(op));
                match operator.and_then(Self::short_circuit_edges) {
                    Some((to_right, to_join)) => self.split(node, current, to_right, to_join),
                    None => self.children(node, current),
                }
            }
            "ternary_expression" => self.ternary(node, current),
            kind if POSTFIX_KINDS.contains(&kind)
                && Self::is_chain_root(node)
                && Self::chain_has_optional(node) =>
            {
                self.postfix_chain(node, current)
            }
            _ => self.children(node, current),
        }
    }

    /// Evaluate every named child in order, with no branch of this node's own.
    fn children(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        let mut block = current;
        for child in children {
            block = self.expression(child, block);
        }
        block
    }

    /// `left <op> right`, where `<op>` decides whether `right` is evaluated.
    fn split(
        &mut self,
        node: Node<'t>,
        current: BlockId,
        to_right: EdgeKind,
        to_join: EdgeKind,
    ) -> BlockId {
        let Some(left) = node.child_by_field_name("left") else {
            return self.children(node, current);
        };
        let Some(right) = node.child_by_field_name("right") else {
            return self.children(node, current);
        };
        let test = self.expression(left, current);
        self.cfg.attribute(test, left);

        let right_entry = self.cfg.alloc(right.start_byte());
        let join = self.cfg.alloc(node.end_byte());
        self.cfg.edge(test, right_entry, to_right, false);
        self.cfg.edge(test, join, to_join, false);

        let right_end = self.expression(right, right_entry);
        self.cfg.attribute(right_end, right);
        self.cfg.edge(right_end, join, EdgeKind::Normal, false);
        join
    }

    fn ternary(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        let (Some(condition), Some(consequence), Some(alternative)) = (
            node.child_by_field_name("condition"),
            node.child_by_field_name("consequence"),
            node.child_by_field_name("alternative"),
        ) else {
            return self.children(node, current);
        };
        let test = self.expression(condition, current);
        self.cfg.attribute(test, condition);

        let join = self.cfg.alloc(node.end_byte());
        for (arm, kind) in [
            (consequence, EdgeKind::True),
            (alternative, EdgeKind::False),
        ] {
            let entry = self.cfg.alloc(arm.start_byte());
            self.cfg.edge(test, entry, kind, false);
            let end = self.expression(arm, entry);
            self.cfg.attribute(end, arm);
            self.cfg.edge(end, join, EdgeKind::Normal, false);
        }
        join
    }

    /// A postfix chain with at least one optional link.
    ///
    /// **One join for the whole chain**, because ECMA-262 short-circuits the chain and not
    /// the link: once `a?.b` yields `undefined`, `a?.b.c` never reads `c` and `a?.b.c()`
    /// never calls. The outer links carry no marker of their own — `a?.b.c` is
    /// `member_expression(object: member_expression(a ?. b), property: c)` — so a join per
    /// optional link would model `c` as reached on both branches.
    ///
    /// `True` continues the chain and `False` reaches the join, per the convention
    /// [`EdgeKind`] states: the condition is "the operand so far is non-nullish".
    fn postfix_chain(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        // The tree spells a chain outermost-first; evaluation runs the other way.
        let mut links = vec![node];
        let mut base = Self::postfix_base(node);
        while let Some(inner) = base.filter(|inner| POSTFIX_KINDS.contains(&inner.kind())) {
            links.push(inner);
            base = Self::postfix_base(inner);
        }
        links.reverse();
        // `base` is now what the innermost link applies to, and by construction it is not
        // itself a link — so nothing here re-dispatches a node this loop already owns.

        let mut block = current;
        if let Some(base) = base {
            block = self.expression(base, block);
            self.cfg.attribute(block, base);
        }

        let join = self.cfg.alloc(node.end_byte());
        for link in links {
            let marker = Self::optional_marker(link);
            if let Some(marker) = marker {
                let rest = self.cfg.alloc(marker.end_byte());
                self.cfg.edge(block, rest, EdgeKind::True, false);
                self.cfg.edge(block, join, EdgeKind::False, false);
                block = rest;
            }
            // This link's own operands: the property, the index, the type arguments, the
            // arguments. Not its base, which the link below it already evaluated, and not
            // the `optional_chain` marker — that one is a *named* node despite being
            // punctuation, so leaving it in would put `?.` into `Block::nodes`.
            let skip = [Self::postfix_base(link), marker].map(|found| found.map(|n| n.id()));
            let mut cursor = link.walk();
            let operands: Vec<Node<'t>> = link
                .named_children(&mut cursor)
                .filter(|child| !skip.contains(&Some(child.id())))
                .collect();
            for operand in operands {
                block = self.expression(operand, block);
                self.cfg.attribute(block, operand);
            }
            self.cfg.attribute(block, link);
        }

        self.cfg.edge(block, join, EdgeKind::Normal, false);
        join
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

    /// The kinds attributed to the block `node` resolves to.
    ///
    /// **Assert on this, never on `block_of(node).is_some()`.** `block_of` resolves by
    /// range containment, so an attribution that is too *wide* — the whole `else_clause`
    /// instead of the statement inside it, the whole `with_statement` instead of its body
    /// — still contains the node's start byte and still answers `Some`. Four of this
    /// task's six mutations survived on exactly that.
    fn attributed<'a>(cfg: &'a Cfg<'_>, node: tree_sitter::Node<'_>) -> Vec<&'a str> {
        let id = cfg.block_of(node).expect("the node resolves to a block");
        cfg.block(id)
            .nodes
            .iter()
            .map(tree_sitter::Node::kind)
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
    fn every_function_like_root_kind_builds() {
        // Pins all six function-like `ROOT_KINDS` entries, not just `function_declaration`
        // and `arrow_function`, which is all the rest of this file's tests happen to cover.
        // A kind added to `ROOT_KINDS` without a `body` field would fail silently — `build`
        // returns `Some` either way, just with an empty walk — so this checks that each
        // kind's body is actually walked, not merely that `build` accepts the kind.
        for (source, kind) in [
            ("function f() { a(); }", "function_declaration"),
            ("function* f() { a(); }", "generator_function_declaration"),
            ("const f = function () { a(); };", "function_expression"),
            ("const f = function* () { a(); };", "generator_function"),
            ("const f = () => { a(); };", "arrow_function"),
            ("class C { m() { a(); } }", "method_definition"),
        ] {
            let tree = parse(source);
            let cfg = Cfg::build(source, find(&tree, kind))
                .unwrap_or_else(|| panic!("`{kind}` is a root"));
            assert!(
                attributed(&cfg, find(&tree, "expression_statement"))
                    .contains(&"expression_statement"),
                "`{kind}`: body was not walked",
            );
        }
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
        // the body, which is exactly what this asserts against. `block_of(call).is_some()`
        // is not enough: an unwrapped `else_clause` attributes the whole clause (rather
        // than the statement inside it) to the same block, and that wider attribution
        // still contains the statement's start byte, so presence alone would pass either
        // way. Asserting the attributed *kind* is what actually distinguishes them.
        let source = "function f() { if (c) { a(); } else { b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let calls = find_all(&tree, "expression_statement");
        assert_eq!(calls.len(), 2);
        for call in calls {
            assert!(
                attributed(&cfg, call).contains(&"expression_statement"),
                "`{}` was not attributed as itself",
                call.kind()
            );
        }
    }

    #[test]
    fn an_else_if_chains_rather_than_nesting_a_clause() {
        // Same weakness as the test above: `block_of(call).is_some()` would also pass if
        // an `else if` clause were attributed whole instead of unwrapped, since the call
        // still falls inside that wider range.
        let source = "function f() { if (a) { x(); } else if (b) { y(); } else { z(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        for call in find_all(&tree, "expression_statement") {
            assert!(attributed(&cfg, call).contains(&"expression_statement"));
        }
    }

    #[test]
    fn both_arms_returning_leaves_the_join_unreachable() {
        // Not a predecessor-count check on `join`: `join`'s predecessor list is populated
        // solely by the two `if let Some(tail) = self.statement(...)` bodies, which are
        // already gated on whether each arm falls through — `reachable` never touches an
        // edge itself, it only decides what `if_statement` returns to *its* caller. So a
        // broken `reachable` (e.g. forced `true` unconditionally) is invisible to `join`'s
        // predecessors and to everything else in this block: the entire test binary passed
        // against that mutation. The only place the bookkeeping is externally visible is
        // whether the *next* statement after the `if` gets walked at all.
        let source = "function f() { if (c) { return 1; } else { return 2; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        assert!(
            cfg.block_of(find(&tree, "call_expression")).is_none(),
            "`after()` is unreachable, so nothing should have walked it",
        );
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
        // Deleting the early return that stops `expression` at a nested function collapses
        // the *entire* body into one block, since nothing inside any expression walk is
        // attributed individually any more — only the enclosing statement is, by
        // `statement`'s catch-all. Under that collapse `block_of(inner) == block_of(arrow)`
        // above holds vacuously (everything shares the one block), so it takes asserting
        // that the arrow itself was attributed to actually require the early return to
        // have fired.
        assert!(
            attributed(&cfg, arrow).contains(&"arrow_function"),
            "the arrow itself must be attributed, not merely reachable from the same block",
        );
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
        // The body is a single implicit return: it is attributed, and the block it
        // completes in is the one that flows to the exit. Not `!successors.is_empty()` on
        // the entry — `build` wires `entry -> start` before it looks at the body at all,
        // so that edge exists for every root whether the body was handled or not.
        let body = cfg
            .block_of(find(&tree, "binary_expression"))
            .expect("the body is walked");
        let targets: Vec<BlockId> = cfg
            .block(body)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets, vec![cfg.exit()]);
    }

    #[test]
    fn a_with_statement_walks_its_body_as_statements() {
        // Deprecated, and it parses, so omitting it drops every statement inside one.
        //
        // Not an edge-to-`exit` assertion: this fixture's `return` is the function's sole
        // and last statement, so even an opaque, un-unwrapped `with_statement` — attributed
        // whole, with `return_statement` never walked as a statement at all — still ends up
        // on a block whose only successor is `exit`, via the ordinary "fell off the end of
        // the function" edge. That coincidence is exactly what let this test pass with the
        // `"with_statement"` arm deleted outright. Asserting the attributed *kind* is what
        // separates "walked as statements" from "walked as one opaque expression".
        let source = "function f(o) { with (o) { return 1; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        assert!(
            attributed(&cfg, find(&tree, "return_statement")).contains(&"return_statement"),
            "the `return` inside `with` must be attributed as itself, not folded into an \
             opaque `with_statement`",
        );
    }

    #[test]
    fn a_with_statement_evaluates_its_object() {
        // `object` is a required field on `with_statement`, and it must be walked: without
        // that, `with (a && b) { ... }` would silently drop Task 3's short-circuit edges
        // inside the object expression. A bare identifier or an unsplit `a && b` can't make
        // that observable yet, though — Task 2's placeholder `expression` is a no-op on
        // anything that isn't a nested function (its one early return is the only thing it
        // does), so the object needs a nested function inside it before "walked" versus
        // "skipped" produces any difference at all.
        let source = "function f() { with (() => { inner(); }) { return 1; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let arrow = find(&tree, "arrow_function");
        assert!(
            attributed(&cfg, arrow).contains(&"arrow_function"),
            "the `with` object must be walked, not skipped in favor of the body alone",
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

    /// Whether any path from `from` to `to` avoids every block attributed `kind`.
    fn skips(cfg: &Cfg<'_>, from: BlockId, to: BlockId, avoided: tree_sitter::Node<'_>) -> bool {
        let banned = cfg.block_of(avoided);
        let mut seen = vec![false; cfg.blocks().count()];
        let mut stack = vec![from];
        while let Some(id) = stack.pop() {
            if id == to {
                return true;
            }
            for edge in &cfg.block(id).successors {
                if Some(edge.target) == banned || seen[edge.target.index()] {
                    continue;
                }
                seen[edge.target.index()] = true;
                stack.push(edge.target);
            }
        }
        false
    }

    #[test]
    fn and_short_circuits_past_its_right_operand() {
        let source = "function f() { const x = a && b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let right = find_all(&tree, "identifier")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "b")
            .unwrap();
        assert!(
            skips(&cfg, cfg.entry(), cfg.exit(), right),
            "`b` must be skippable when `a` is falsy",
        );
    }

    #[test]
    fn and_labels_true_toward_the_right_operand() {
        let source = "function f() { const x = a && b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let test = cfg.block_of(ident("a")).unwrap();
        let right = cfg.block_of(ident("b")).unwrap();
        // Direction, not membership. Asserting only that a True and a False edge exist
        // lets the whole label convention be inverted with nothing noticing — and #193
        // and #194 read these labels off the graph.
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(test)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::True),
            vec![right],
            "`&&` evaluates its right operand when true"
        );
        assert_ne!(to(EdgeKind::False), vec![right], "`&&` skips it when false");
    }

    #[test]
    fn nullish_coalescing_short_circuits_past_its_right_operand() {
        let source = "function f() { const x = a ?? b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let right = find_all(&tree, "identifier")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "b")
            .unwrap();
        assert!(skips(&cfg, cfg.entry(), cfg.exit(), right));
    }

    #[test]
    fn nullish_coalescing_labels_false_toward_the_right_operand() {
        // The opposite polarity to `&&`, and the reason row 3's mutation was survivable:
        // the condition is "the left operand is non-nullish", so `False` is the edge that
        // evaluates the right-hand side.
        let source = "function f() { const x = a ?? b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let test = cfg.block_of(ident("a")).unwrap();
        let right = cfg.block_of(ident("b")).unwrap();
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(test)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::False),
            vec![right],
            "`??` evaluates its right operand when the left is nullish"
        );
        assert_ne!(
            to(EdgeKind::True),
            vec![right],
            "`??` skips it when the left is non-nullish"
        );
    }

    #[test]
    fn a_non_short_circuiting_binary_operator_does_not_split() {
        // `in` and `instanceof` are in the same operator set and must not branch.
        let source = "function f() { const x = a instanceof b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let right = find_all(&tree, "identifier")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "b")
            .unwrap();
        assert!(
            !skips(&cfg, cfg.entry(), cfg.exit(), right),
            "`instanceof` must not branch"
        );
    }

    #[test]
    fn a_ternary_branches_like_an_if() {
        let source = "function f() { const x = c ? a : b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        assert!(skips(&cfg, cfg.entry(), cfg.exit(), ident("a")));
        assert!(skips(&cfg, cfg.entry(), cfg.exit(), ident("b")));
    }

    #[test]
    fn an_optional_chain_short_circuits_past_the_rest_of_the_chain() {
        let source = "function f() { const x = a?.b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let member = find(&tree, "member_expression");
        let property = member.child_by_field_name("property").unwrap();
        assert!(skips(&cfg, cfg.entry(), cfg.exit(), property));
    }

    #[test]
    fn a_plain_member_access_does_not_split() {
        let source = "function f() { const x = a.b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let property = find(&tree, "member_expression")
            .child_by_field_name("property")
            .unwrap();
        assert!(!skips(&cfg, cfg.entry(), cfg.exit(), property));
    }
    #[test]
    fn an_optional_call_short_circuits_past_its_arguments() {
        // `call_expression` carries neither an `optional_chain` field nor a child of that
        // kind — TypeScript overrides the base call rule and spells the optional form as a
        // bare `?.` token — so a detector written from `node-types.json` alone models both
        // of these commonplace idioms as unconditional calls.
        for source in [
            "function outer() { const x = f?.(); }",
            "function outer() { const x = obj.method?.(); }",
        ] {
            let tree = parse(source);
            let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
            // `formal_parameters`, not `arguments`, is the declaration's own list, so this
            // is the call's. The `call_expression` node itself cannot be used here: it
            // starts where its callee does, and `block_of` resolves by range containment,
            // so it would answer with the callee's block rather than the call's.
            let arguments = find(&tree, "arguments");
            assert!(
                attributed(&cfg, arguments).contains(&"call_expression"),
                "{source}: the arguments must resolve to the block holding the call",
            );
            assert!(
                skips(&cfg, cfg.entry(), cfg.exit(), arguments),
                "{source}: the call must be skippable when the callee is nullish",
            );
        }
    }

    #[test]
    fn an_optional_chain_short_circuits_every_later_link() {
        // `a?.b.c` is `member_expression(object: member_expression(a ?. b), property: c)`.
        // The outer `.c` carries no marker of its own, so a join per *link* leaves `c`
        // walked on both branches — reached even when `a` is nullish, which ECMA-262 says
        // it is not.
        let source = "function f() { const x = a?.b.c; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let property = |name: &str| {
            find_all(&tree, "property_identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        assert_eq!(
            attributed(&cfg, property("c")),
            vec![
                "property_identifier",
                "member_expression",
                "property_identifier",
                "member_expression",
            ],
            "both links belong to the one block the chain continues into",
        );
        assert!(
            skips(&cfg, cfg.entry(), cfg.exit(), property("c")),
            "`c` is not read when `a` is nullish",
        );
        // The marker is a *named* node despite being punctuation, so an unfiltered
        // `named_children` walk puts `?.` into `Block::nodes`.
        assert!(
            !shape(&cfg).concat().contains(&"optional_chain"),
            "`?.` is punctuation and must not be attributed",
        );
    }

    #[test]
    fn an_optional_chain_short_circuits_past_a_later_call() {
        // The worse half of the same defect: modelled per-link, `a?.b.c()` makes the call
        // happen unconditionally.
        let source = "function f() { const x = a?.b.c(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let arguments = find(&tree, "arguments");
        assert!(
            attributed(&cfg, arguments).contains(&"call_expression"),
            "the arguments must resolve to the block holding the call",
        );
        assert!(
            skips(&cfg, cfg.entry(), cfg.exit(), arguments),
            "the call must not happen when `a` is nullish",
        );
    }

    #[test]
    fn an_optional_link_labels_true_toward_the_rest_of_the_chain() {
        // Direction, not membership. `??` and `?.` share a condition — "the left operand
        // is non-nullish" — and land on opposite labels: `??` evaluates its right operand
        // when the condition fails, `?.` continues the chain when it holds.
        let source = "function f() { const x = a?.b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let chain = find(&tree, "member_expression");
        let object = chain.child_by_field_name("object").unwrap();
        let property = chain.child_by_field_name("property").unwrap();
        assert_eq!(
            attributed(&cfg, object),
            vec!["identifier"],
            "`a` is evaluated before the branch, in its own block",
        );
        assert_eq!(
            attributed(&cfg, property),
            vec!["property_identifier", "member_expression"],
            "`b` belongs to the block the chain continues into",
        );
        let test = cfg.block_of(object).unwrap();
        let rest = cfg.block_of(property).unwrap();
        let join = cfg
            .block(rest)
            .successors
            .iter()
            .find(|e| e.kind == EdgeKind::Normal)
            .map(|e| e.target)
            .expect("the rest of the chain rejoins");
        assert_ne!(rest, join, "the chain's continuation is not its join");
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(test)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::True),
            vec![rest],
            "`?.` continues the chain when the left operand is non-nullish",
        );
        assert_eq!(
            to(EdgeKind::False),
            vec![join],
            "`?.` yields `undefined` and skips to the join when it is nullish",
        );
    }

    #[test]
    fn logical_or_short_circuits_past_its_right_operand() {
        let source = "function f() { const x = a || b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let right = find_all(&tree, "identifier")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "b")
            .unwrap();
        assert_eq!(attributed(&cfg, right), vec!["identifier"]);
        assert!(
            skips(&cfg, cfg.entry(), cfg.exit(), right),
            "`b` must be skippable when `a` is truthy",
        );
    }

    #[test]
    fn logical_or_labels_false_toward_the_right_operand() {
        // `||` shares `short_circuit_edges`'s second arm with `??` and had no test of its
        // own, so splitting that arm and giving `||` the `&&` pair passed the whole file.
        let source = "function f() { const x = a || b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let test = cfg.block_of(ident("a")).unwrap();
        let right = cfg.block_of(ident("b")).unwrap();
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(test)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::False),
            vec![right],
            "`||` evaluates its right operand when the left is falsy",
        );
        assert_ne!(
            to(EdgeKind::True),
            vec![right],
            "`||` skips it when the left is truthy",
        );
    }

    #[test]
    fn a_plain_member_chain_does_not_split() {
        // The chain walk runs over every postfix chain; only an optional one may branch.
        let source = "function f() { const x = a.b.c; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let last = find_all(&tree, "property_identifier")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "c")
            .unwrap();
        assert_eq!(
            attributed(&cfg, last),
            vec!["lexical_declaration"],
            "an unsplit chain is attributed whole, with no fragment blocks",
        );
        assert!(!skips(&cfg, cfg.entry(), cfg.exit(), last));
    }
}
