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
///
/// Any other node standing between two links ends the chain. That is required for
/// `(a?.b).c` and for the index in `x[a?.b]`, where a `parenthesized_expression` or a
/// sibling field genuinely does end it — and it is **wrong for `a?.b!.c`**, a known and
/// accepted gap. TypeScript erases `!` at run time, so that expression is `a?.b.c` and
/// `.c` should be skipped when `a` is nullish. Measured: it is not. `non_null_expression`
/// stops the walk, `a?.b` becomes a chain of its own, and `.c` lands in the join block,
/// which is on every path. Taken deliberately — the remedy is a pass-through for that one
/// kind rather than a general widening, and the error direction is over-reachability,
/// which surfaces downstream as a visible false positive rather than as silence.
const POSTFIX_KINDS: &[&str] = &[
    "member_expression",
    "subscript_expression",
    "call_expression",
];

/// Statement kinds that are loops, and so can house a labeled `continue`.
///
/// A label on one of these — or a chain of labels stacked above one, each wrapping the
/// next — belongs to the loop's own [`Target`], via `pending_labels`, so the loop can
/// answer a labeled `continue`. A label whose body is neither a loop nor another labeled
/// statement gets a break-only `Target` from `labeled_statement` itself, with
/// `continue_to: None` — an unlabeled `continue` then passes through it to the enclosing
/// loop.
const LOOP_KINDS: &[&str] = &[
    "while_statement",
    "do_statement",
    "for_statement",
    "for_in_statement",
];

/// A `break`/`continue` target.
struct Target<'s> {
    /// The labels this target answers to. Empty for an unlabeled construct; more than one
    /// when stacked labels (`a: b: while (c) { ... }`) all name the same loop.
    labels: Vec<&'s str>,
    break_to: BlockId,
    /// `None` for a `switch` or a labeled block, which `continue` passes through.
    continue_to: Option<BlockId>,
    /// `self.finallys.len()` when this target was pushed. Jumping here unwinds
    /// everything pushed since — and it is also what [`Builder::emit_finally_copy`] cuts
    /// this stack on, so a finalizer copy cannot answer a jump with a loop that was opened
    /// inside the `try` the finalizer belongs to.
    finally_depth: usize,
}

/// A `finally` clause still in scope, and the copies already emitted for it.
struct Pending<'t> {
    /// The `statement_block` that is the clause's body.
    body: Node<'t>,
    /// Continuation to copy-entry. A list rather than a map: it holds a handful of
    /// entries, and iterating a hash container is what the determinism requirement
    /// forbids.
    memo: Vec<(BlockId, BlockId)>,
}

/// Where a `throw` goes.
struct Handler {
    to: BlockId,
    /// Finally levels to unwind before reaching `to`.
    ///
    /// Recorded *after* the enclosing `try`'s own finalizer is pushed, so that finalizer
    /// is above this depth and does not run on the body-to-`catch` path: `finally` runs
    /// after `catch`, not instead of it. Being above it is also what takes this handler
    /// out of scope inside a copy of that finalizer, in [`Builder::emit_finally_copy`].
    finally_depth: usize,
}

struct Builder<'t, 's> {
    cfg: Cfg<'t>,
    /// Read only by `text`.
    source: &'s str,
    targets: Vec<Target<'s>>,
    /// The `finally` clauses enclosing the statement being walked, outermost first.
    ///
    /// Read for its length, to stamp `Target::finally_depth` and `Handler::finally_depth`,
    /// and read back by [`Builder::unwind`], which copies every level from a jump's
    /// recorded depth outward.
    finallys: Vec<Pending<'t>>,
    /// The `catch` clauses enclosing the statement being walked, outermost first.
    handlers: Vec<Handler>,
    /// Labels stacked above the construct they belong to, outermost first.
    ///
    /// A field rather than a parameter: it would otherwise thread through `statement`,
    /// which every construct calls and none but `labeled_statement` needs.
    /// `labeled_statement` pushes onto it and recurses when its own body is itself a loop
    /// or another labeled statement, so `a: b: while (c) {}` accumulates both labels
    /// before the `while` constructor drains them into its `Target` in one call. Drained
    /// (via `std::mem::take`) by whichever constructor ends the chain — a loop
    /// constructor, or `labeled_statement` itself for a non-loop body — which is what
    /// keeps it empty for the next, unrelated labeled statement.
    pending_labels: Vec<&'s str>,
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
    ///
    /// **The two parameters can disagree, and the caller is what pairs them.** `root` must
    /// be a node of the tree `source` was parsed from: construction slices `source` by node
    /// byte range, so a `source` shorter than the tree indexes out of bounds. Also `None`
    /// for that, which turns a panic wherever the walk first reads text into a refusal at
    /// the call.
    ///
    /// It closes one shape and not the hazard. A *different* string of the same length is
    /// still accepted, and what happens then depends on the bytes: every read whose range
    /// lands on a `char` boundary slices successfully and answers about text nobody parsed,
    /// while one that does not panics with `byte index N is not a char boundary`. So the
    /// remaining failure is silent on ASCII and loud on multibyte UTF-8 — the worse of those
    /// being the silent one. No cheap check distinguishes the pair; it stays a caller error,
    /// named here rather than left to be discovered.
    #[must_use]
    pub fn build(source: &str, root: Node<'t>) -> Option<Self> {
        if !ROOT_KINDS.contains(&root.kind()) {
            return None;
        }
        if root.end_byte() > source.len() {
            return None;
        }
        let mut builder = Builder {
            cfg: Self::new_empty(root.byte_range()),
            source,
            targets: Vec::new(),
            finallys: Vec::new(),
            handlers: Vec::new(),
            pending_labels: Vec::new(),
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

/// The nearest ancestor (or `node` itself) that a [`Cfg`] can be built from — a function
/// or the program. The single reader of [`ROOT_KINDS`] outside construction, so the set of
/// function-like kinds is defined once (AGENTS.md records `SCOPE_KINDS` drifting when a
/// second copy was maintained by hand).
#[must_use]
pub(crate) fn enclosing_cfg_root(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if ROOT_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// The nearest `statement_block` ancestor of `node`, the lexical region `scope: 'block'`
/// bounds an obligation to.
#[must_use]
pub(crate) fn enclosing_block(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "statement_block" {
            return Some(n);
        }
        current = n.parent();
    }
    None
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
            // The edge to the exit goes through every pending `finally`, which is the
            // whole of "a `return` inside a `try` runs the finalizer first" — there is no
            // case for `try` here.
            "return_statement" => {
                let end = match Self::operand(node) {
                    Some(operand) => self.expression(operand, current),
                    None => current,
                };
                self.cfg.attribute(end, node);
                let exit = self.cfg.exit();
                let target = self.unwind(0, exit);
                self.cfg.edge(end, target, EdgeKind::Normal, false);
                None
            }
            // The only kind that emits an `Exception` edge. A call that may throw
            // deliberately gets none (#192's design, §4.8): edges from every
            // `call_expression` would report `acquire(); doWork(); release()` in every
            // function that does not wrap it, which is true and unusable. Widening it is
            // #195's decision, and this is the one place that would change.
            "throw_statement" => {
                let end = match Self::operand(node) {
                    Some(operand) => self.expression(operand, current),
                    None => current,
                };
                self.cfg.attribute(end, node);
                let (to, depth) = self.innermost_handler();
                let target = self.unwind(depth, to);
                self.cfg.edge(end, target, EdgeKind::Exception, false);
                None
            }
            "try_statement" => self.try_statement(node, current),
            "while_statement" => Some(self.while_statement(node, current)),
            "do_statement" => self.do_statement(node, current),
            "for_statement" => Some(self.for_statement(node, current)),
            "for_in_statement" => Some(self.for_in_statement(node, current)),
            "labeled_statement" => self.labeled_statement(node, current),
            "break_statement" => self.jump(node, current, false),
            "continue_statement" => self.jump(node, current, true),
            "switch_statement" => self.switch_statement(node, current),
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
    /// **Two spellings, and the grammars disagree about which kinds use which.** A marker is
    /// either a *named* node of kind `optional_chain`, or a bare *anonymous* `?.` token.
    ///
    /// - `member_expression` and `subscript_expression` declare an `optional_chain` field in
    ///   every grammar registered here, and its value is the named node.
    /// - `call_expression` declares that field in `tree-sitter-javascript` 0.25.0
    ///   (`grammar.js:868`), which `register_all` registers and whose grammar also covers
    ///   JSX — but **not** in `tree-sitter-typescript` 0.23.2, where `common/define-grammar.js`
    ///   overrides the base call rule to
    ///   `seq(field('function', ..), '?.', field('type_arguments', ..), field('arguments', ..))`.
    ///   In TypeScript and TSX its declared fields are `function`, `type_arguments`,
    ///   `arguments`, and the anonymous token is the only marker there is. Missing that is
    ///   why `f?.()` and `obj.method?.()` were once modeled as unconditional calls.
    ///
    /// **The field read below is a fast path, not the second of two required cases**, and it
    /// is worth saying so where someone might otherwise restore the opposite claim. In all
    /// three `node-types.json` files the field's value is a named child of kind
    /// `optional_chain`, so the scan already reaches everything the field read reaches.
    /// Measured, by deleting each arm in turn: without the field read the whole suite stays
    /// green; without the scan, `an_optional_call_short_circuits_past_its_arguments` fails,
    /// alone — it is the one fixture whose marker is anonymous. (A count of passing tests
    /// would go stale here; the named test does not.) Kept because naming the declared field states the
    /// intent, and because it would still answer correctly for a grammar that declared the
    /// field with some other value kind. The scan is the arm that must not be deleted.
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
            // An inner link genuinely completes where it is; the chain as a whole completes
            // at the join, where the short-circuited and the completed paths meet. "Where
            // does this expression complete?" is the question a consumer asks most often,
            // and it has to have one answer — attributing the outermost link to the block
            // its own evaluation reached would put it in two blocks the moment the chain is
            // an operand of an enclosing split, which attributes it to the join as well.
            let completes_at = if link.id() == node.id() { join } else { block };
            self.cfg.attribute(completes_at, link);
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

    /// The block a `break`/`continue` goes to, with the finally depth to unwind first.
    ///
    /// Innermost-first. An unlabeled `continue` skips targets that answer only `break` —
    /// a `switch`, a labeled block — which is why the two are separate searches rather
    /// than one.
    fn jump_target(&self, label: Option<&str>, is_continue: bool) -> Option<(BlockId, usize)> {
        self.targets.iter().rev().find_map(|target| {
            if let Some(label) = label
                && !target.labels.contains(&label)
            {
                return None;
            }
            if is_continue {
                target.continue_to.map(|to| (to, target.finally_depth))
            } else {
                Some((target.break_to, target.finally_depth))
            }
        })
    }

    /// Returning `None` for a `break`/`continue` with no enclosing target is correct: the
    /// statement is unreachable-from-here either way, and a parse with no target is not
    /// valid JavaScript.
    fn jump(&mut self, node: Node<'t>, current: BlockId, is_continue: bool) -> Option<BlockId> {
        self.cfg.attribute(current, node);
        let label = node.child_by_field_name("label").map(|n| self.text(n));
        // A jump out of a `try` runs every `finally` it leaves, and none of the ones it
        // stays inside: `depth` is how many were already pending when the target was
        // pushed, so `unwind` copies exactly the levels opened since.
        let (to, depth) = self.jump_target(label, is_continue)?;
        let to = self.unwind(depth, to);
        // A `continue` is a back edge and a `break` is not. Comparing ids here would read
        // *allocation* order, since `finish` has not renumbered anything yet.
        self.cfg.edge(current, to, EdgeKind::Normal, is_continue);
        None
    }

    /// The common shape: a header, a body, and an exit.
    ///
    /// `header` and `test` coincide for a non-branching condition and diverge whenever the
    /// condition itself does: `while (a && b)` evaluates `a` in `header`, and `split`
    /// emits `a`'s own True/False *from* `header`. The loop's own True/False — body versus
    /// `after` — must come from `test`, the block where the *whole* condition's evaluation
    /// completes (its join, for a compound one), or `header` would carry two conflicting
    /// edges per label: one from the condition's internal branch, one from the loop's.
    /// `header` stays right for the back edge and `continue_to` — re-testing the whole
    /// condition from its start on a `continue` is correct.
    ///
    /// `conditional` is false for `for (;;)`, which has no test and therefore no way to
    /// fall out. Emitting a `False` edge there would invent a path the program does not
    /// have — and would make every obligation inside such a loop look dischargeable.
    /// No constant folding: `while (true)` has a condition, so it keeps both edges.
    fn loop_statement(
        &mut self,
        node: Node<'t>,
        header: BlockId,
        test: BlockId,
        continue_to: BlockId,
        labels: Vec<&'s str>,
        conditional: bool,
    ) -> BlockId {
        let after = self.cfg.alloc(node.end_byte());
        let body = node.child_by_field_name("body");
        let body_entry = self
            .cfg
            .alloc(body.map_or(node.end_byte(), |b| b.start_byte()));
        self.cfg.edge(test, body_entry, EdgeKind::True, false);
        if conditional {
            self.cfg.edge(test, after, EdgeKind::False, false);
        }

        self.targets.push(Target {
            labels,
            break_to: after,
            continue_to: Some(continue_to),
            finally_depth: self.finallys.len(),
        });
        let tail = body.and_then(|b| self.statement(b, body_entry));
        self.targets.pop();

        if let Some(tail) = tail {
            self.cfg
                .edge(tail, continue_to, EdgeKind::Normal, continue_to == header);
        }
        after
    }

    /// Never `None`: a `while` always produces its `after` block, whether or not it is
    /// reachable. Reachability is a fact about `after`'s predecessor count, not about this
    /// return value — the same "no constant folding" stance `loop_statement` documents.
    fn while_statement(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        let condition = node.child_by_field_name("condition");
        // Anchored on the condition rather than on the statement, so the header cannot tie
        // with a block the enclosing construct allocated at the same offset.
        let header = self
            .cfg
            .alloc(condition.map_or(node.start_byte(), |c| c.start_byte()));
        self.cfg.edge(current, header, EdgeKind::Normal, false);
        let mut test = header;
        if let Some(condition) = condition {
            let end = self.expression(condition, header);
            self.cfg.attribute(end, condition);
            // Same reasoning as `if_statement`: without this, `block_of(while_statement)`
            // answers `None`, since the node's own start byte (the `while` keyword)
            // precedes everything attributed inside it.
            self.cfg.attribute(end, node);
            test = end;
        }
        let labels = std::mem::take(&mut self.pending_labels);
        self.loop_statement(node, header, test, header, labels, true)
    }

    fn do_statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        let body = node.child_by_field_name("body")?;
        let body_entry = self.cfg.alloc(body.start_byte());
        self.cfg.edge(current, body_entry, EdgeKind::Normal, false);

        let condition = node.child_by_field_name("condition");
        let latch = self
            .cfg
            .alloc(condition.map_or(node.end_byte(), |c| c.start_byte()));
        let after = self.cfg.alloc(node.end_byte());

        self.targets.push(Target {
            labels: std::mem::take(&mut self.pending_labels),
            break_to: after,
            continue_to: Some(latch),
            finally_depth: self.finallys.len(),
        });
        let tail = self.statement(body, body_entry);
        self.targets.pop();

        if let Some(tail) = tail {
            self.cfg.edge(tail, latch, EdgeKind::Normal, false);
        }
        let mut test = latch;
        if let Some(condition) = condition {
            let end = self.expression(condition, latch);
            self.cfg.attribute(end, condition);
            // Same reasoning as `if_statement`: without this, `block_of(do_statement)`
            // answers `None`, since the node's own start byte (the `do` keyword) precedes
            // everything attributed inside it.
            self.cfg.attribute(end, node);
            test = end;
        }
        // The one edge that is both a back edge and a true branch, which is why `back` is
        // a field on `Edge` rather than a variant of `EdgeKind`. Emitted from `test` —
        // where the *whole* condition's evaluation completes — rather than `latch`, which
        // would otherwise also carry a conflicting pair of True/False edges from the
        // condition's own internal branch whenever it short-circuits.
        self.cfg.edge(test, body_entry, EdgeKind::True, true);
        self.cfg.edge(test, after, EdgeKind::False, false);
        Some(after)
    }

    /// `for_statement`'s `condition` and `initializer` fields, unlike `increment`, are
    /// declared `required` in the grammar — so an omitted clause is not an absent field.
    /// It is a present `empty_statement` placeholder at the clause's position. Measured
    /// against `tree-sitter-typescript` 0.23.2: `for (;;)` gives
    /// `condition = Some(empty_statement)` and `initializer = Some(empty_statement)`, not
    /// `None`. Trusting `child_by_field_name(..).is_some()` for either would make
    /// `for (;;)` read as conditional, inventing a `False` edge out of a loop that has no
    /// way to fall out. `increment` never produces the placeholder — it is genuinely
    /// optional at the grammar level — so filtering it the same way is a no-op there.
    fn for_clause(node: Node<'t>, field: &str) -> Option<Node<'t>> {
        node.child_by_field_name(field)
            .filter(|clause| clause.kind() != "empty_statement")
    }

    /// Never `None`, for the same reason as [`Self::while_statement`].
    fn for_statement(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        let mut block = current;
        if let Some(initializer) = Self::for_clause(node, "initializer") {
            block = self.expression(initializer, block);
            self.cfg.attribute(block, initializer);
        }
        let condition = Self::for_clause(node, "condition");
        let header = self
            .cfg
            .alloc(condition.map_or(node.end_byte(), |c| c.start_byte()));
        self.cfg.edge(block, header, EdgeKind::Normal, false);
        // Same reasoning as `if_statement`: without this, `block_of(for_statement)`
        // answers `None`, since the node's own start byte (the `for` keyword) precedes
        // everything attributed inside it. Falls back to `header` itself when there is no
        // condition to evaluate (`for (;;)`) — the block still exists and is still the
        // construct's own decision point, even though nothing runs there.
        let test = match condition {
            Some(condition) => {
                let end = self.expression(condition, header);
                self.cfg.attribute(end, condition);
                end
            }
            None => header,
        };
        self.cfg.attribute(test, node);

        let increment = Self::for_clause(node, "increment");
        let increment_entry = self
            .cfg
            .alloc(increment.map_or(node.end_byte(), |i| i.start_byte()));
        if let Some(increment) = increment {
            let end = self.expression(increment, increment_entry);
            self.cfg.attribute(end, increment);
        }
        // `continue` targets the increment, not the header: skipping it would turn every
        // `continue` in a counted loop into an infinite loop, and nothing else would say so.
        self.cfg
            .edge(increment_entry, header, EdgeKind::Normal, true);

        let labels = std::mem::take(&mut self.pending_labels);
        self.loop_statement(
            node,
            header,
            test,
            increment_entry,
            labels,
            condition.is_some(),
        )
    }

    /// Never `None`, for the same reason as [`Self::while_statement`].
    fn for_in_statement(&mut self, node: Node<'t>, current: BlockId) -> BlockId {
        let mut block = current;
        if let Some(right) = node.child_by_field_name("right") {
            block = self.expression(right, block);
            self.cfg.attribute(block, right);
        }
        // One kind for `for...in` and `for...of` alike; the `operator` field says which,
        // and neither the edges nor the blocks differ.
        let header = self.cfg.alloc(
            node.child_by_field_name("left")
                .map_or(node.start_byte(), |l| l.start_byte()),
        );
        self.cfg.edge(block, header, EdgeKind::Normal, false);
        // Same reasoning as `if_statement`: without this, `block_of(for_in_statement)`
        // answers `None`, since the node's own start byte (the `for` keyword) precedes
        // everything attributed inside it. There is no condition expression to anchor
        // on — the test is the implicit "does the iteration have a next value" — so this
        // attributes directly to `header`.
        self.cfg.attribute(header, node);
        let labels = std::mem::take(&mut self.pending_labels);
        self.loop_statement(node, header, header, header, labels, true)
    }

    fn labeled_statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        let label = node.child_by_field_name("label").map(|n| self.text(n));
        let body = node.child_by_field_name("body")?;
        if let Some(label) = label {
            self.pending_labels.push(label);
        }

        // A label on a loop, or on another label that itself eventually wraps one, belongs
        // to that loop's own target — `pending_labels` accumulates through the chain so
        // `a: b: while (c) {}` reaches the loop's `Target` with both labels at once.
        // Anything else ends the chain here and gets a break-only target for everything
        // accumulated so far.
        if LOOP_KINDS.contains(&body.kind()) || body.kind() == "labeled_statement" {
            return self.statement(body, current);
        }

        let after = self.cfg.alloc(node.end_byte());
        self.targets.push(Target {
            labels: std::mem::take(&mut self.pending_labels),
            break_to: after,
            continue_to: None,
            finally_depth: self.finallys.len(),
        });
        // Same reasoning as `if_statement`: without this, `block_of(labeled_statement)`
        // answers `None` for a label that isn't on a loop, since the node's own start byte
        // (the label) precedes everything attributed inside `body`.
        self.cfg.attribute(current, node);
        let tail = self.statement(body, current);
        self.targets.pop();
        if let Some(tail) = tail {
            self.cfg.edge(tail, after, EdgeKind::Normal, false);
        }
        Some(after)
    }

    /// Case tests chain by their `False` edge; fallthrough is a `Normal` edge from one
    /// arm's own fall-through block to the next arm's entry, in source order.
    ///
    /// `switch_case` and `switch_default` are the only two kinds `body`'s named children
    /// can be. Only the former carries a `value`, which is what keeps `default` out of
    /// the test chain — it takes no part in deciding which arm runs, only in *where
    /// control lands* when every test fails.
    ///
    /// **Zero `switch_case` arms is a real, distinct shape** — `switch (x) { default: ...
    /// }`, even `switch (x) {}` — and "every test fails" is vacuously true there: there is
    /// no *last* failing test to hang `default`'s edge off, because `previous_test` never
    /// leaves `None`. The discriminant itself has to reach `default` (or, with no
    /// `default` either, after-switch) directly in that case; see `first` below.
    fn switch_statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        let body = node.child_by_field_name("body")?;
        let mut block = current;
        if let Some(value) = node.child_by_field_name("value") {
            block = self.expression(value, block);
            self.cfg.attribute(block, value);
        }
        // Same reasoning as `if_statement`: without this, `block_of(switch_statement)`
        // answers `None`, since the node's own start byte (the `switch` keyword) precedes
        // everything attributed inside it.
        self.cfg.attribute(block, node);

        let mut cursor = body.walk();
        let arms: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
        let after = self.cfg.alloc(node.end_byte());

        // One entry block per arm, allocated up front so a fallthrough edge can name the
        // next arm before it has been walked.
        let entries: Vec<BlockId> = arms
            .iter()
            .map(|arm| self.cfg.alloc(arm.start_byte()))
            .collect();

        // A `default` need not be last, so it is found by position rather than assumed to
        // be the chain's tail. Computed before the test chain below: the zero-`switch_case`
        // shape needs it to route the discriminant directly, since no test ever runs to
        // hand it off.
        let default_entry = arms
            .iter()
            .position(|arm| arm.kind() == "switch_default")
            .map(|index| entries[index]);

        // The test chain, over the `switch_case` arms only. `switch_default` has no
        // `value` and takes no part in it.
        let mut previous_test: Option<BlockId> = None;
        let mut chain_start: Option<BlockId> = None;
        for (index, arm) in arms.iter().enumerate() {
            let Some(value) = arm.child_by_field_name("value") else {
                continue;
            };
            let test = self.cfg.alloc(value.start_byte());
            let end = self.expression(value, test);
            self.cfg.attribute(end, value);
            self.cfg.edge(end, entries[index], EdgeKind::True, false);
            match previous_test {
                Some(previous) => self.cfg.edge(previous, test, EdgeKind::False, false),
                None => chain_start = Some(test),
            }
            previous_test = Some(end);
        }

        // Case tests exist: enter the chain. None but a `default` exists: its body always
        // runs, so the discriminant reaches it directly. Neither (an empty switch body):
        // nothing to run.
        let first = chain_start.or(default_entry).unwrap_or(after);
        self.cfg.edge(block, first, EdgeKind::Normal, false);

        // The *final* failing test's `False` edge reaches `default` when the switch has
        // one, and after-switch otherwise. Only fires when there was at least one test to
        // fail from — the zero-`switch_case` shape is handled by `first` above instead.
        if let Some(last) = previous_test {
            self.cfg
                .edge(last, default_entry.unwrap_or(after), EdgeKind::False, false);
        }

        self.targets.push(Target {
            labels: std::mem::take(&mut self.pending_labels),
            break_to: after,
            // A `switch` answers `break` and passes `continue` through to the enclosing
            // loop, which is why this is `None` rather than `Some(after)`.
            continue_to: None,
            finally_depth: self.finallys.len(),
        });
        for (index, arm) in arms.iter().enumerate() {
            let mut arm_cursor = arm.walk();
            let statements: Vec<Node<'t>> = arm
                .children_by_field_name("body", &mut arm_cursor)
                .collect();
            let mut tail = Some(entries[index]);
            for statement in statements {
                let Some(block) = tail else { break };
                tail = self.statement(statement, block);
            }
            // Fallthrough: the next arm's entry in source order, or out of the switch.
            if let Some(tail) = tail {
                let next = entries.get(index + 1).copied().unwrap_or(after);
                self.cfg.edge(tail, next, EdgeKind::Normal, false);
            }
        }
        self.targets.pop();

        Some(after)
    }

    /// The operand of a `return` or a `throw`, which declare no fields.
    ///
    /// Not `named_child(0)`: the grammar's extras are *named* nodes, so
    /// `throw /* why */ a || b;` would hand back the comment, and the `||` split would be
    /// silently absent — an under-approximation with no symptom, since a missing split only
    /// ever removes paths. Every other arm reaches its operand through
    /// `child_by_field_name`, which cannot pick one up; these two have to filter.
    ///
    /// Both of `node-types.json`'s named extras are listed, rather than only the one a
    /// fixture can reach: `html_comment`'s scanner runs to end of line, so it swallows any
    /// operand beside it and a following line is cut off by ASI — no program appears to
    /// reach it. It is named anyway because "the grammar's extras" is the rule, and a list
    /// that happens to match today's reachable cases is the kind that goes quietly stale.
    fn operand(node: Node<'t>) -> Option<Node<'t>> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| !matches!(child.kind(), "comment" | "html_comment"))
    }

    /// Where a `throw` goes, and how many finally levels to unwind first.
    ///
    /// With no enclosing `catch` it leaves the function, and *every* pending finalizer is
    /// on the way — hence depth `0` rather than the current stack height.
    ///
    /// "Enclosing" is whatever [`Self::emit_finally_copy`] left on the stack, which is not
    /// always the lexically enclosing `try`: inside a copy of a finalizer, that `try`'s own
    /// handler has been cut, because a `throw` in a `finally` propagates past the whole
    /// statement rather than into the `catch` beside it.
    fn innermost_handler(&self) -> (BlockId, usize) {
        self.handlers
            .last()
            .map_or((self.cfg.exit(), 0), |handler| {
                (handler.to, handler.finally_depth)
            })
    }

    /// Run every `finally` from `from_depth` outward, ending at `continuation`.
    ///
    /// Returns the entry of the innermost copy, which is where the jump goes — or
    /// `continuation` itself when there is nothing to unwind, which is every jump outside
    /// a `try`. Iterating outward-first means each copy is built pointing at the chain
    /// already built for the levels outside it, so the innermost copy — the last one
    /// built — is the entry.
    fn unwind(&mut self, from_depth: usize, continuation: BlockId) -> BlockId {
        let mut target = continuation;
        for level in from_depth..self.finallys.len() {
            target = self.emit_finally_copy(level, target);
        }
        target
    }

    /// One copy of `finallys[level]` whose normal exit flows to `continuation`.
    ///
    /// Memoized on `continuation`, which is the whole of the "once per distinct
    /// continuation" rule: every `return` in the guarded region shares one copy because
    /// they all continue to the exit.
    fn emit_finally_copy(&mut self, level: usize, continuation: BlockId) -> BlockId {
        if let Some(&(_, entry)) = self.finallys[level]
            .memo
            .iter()
            .find(|(seen, _)| *seen == continuation)
        {
            return entry;
        }
        let body = self.finallys[level].body;
        let entry = self.cfg.alloc(body.start_byte());
        // Recorded before the walk, so a jump inside the copy that shares this
        // continuation reuses it instead of recursing without bound.
        self.finallys[level].memo.push((continuation, entry));

        // All three scope stacks are narrowed, not just `finallys`, and for one reason:
        // this copy is emitted *during* the guarded body's walk, so everything the `try`
        // opened — its own handler, its own finalizer, every loop nested inside it — is
        // still pushed, and none of it is in scope for a finalizer that runs on the way
        // out of the statement. A `return` in the copy must unwind only what encloses the
        // `try`; a `throw` in it is not caught by the `catch` beside it; a `break` or
        // `continue` in it cannot target a loop the finalizer sits outside. Leaving
        // `handlers` and `targets` alone puts edges in the graph for paths the program
        // does not have, which is the invented-path failure this whole construction is
        // shaped to avoid.
        //
        // `finally_depth` separates the two sides: anything the `try` opened recorded a
        // depth *above* `level`, and anything enclosing it recorded `level` or less.
        // Depths are monotone up both stacks — nothing is pushed while an entry recorded
        // at a greater depth is still live — so one position cuts each cleanly.
        let handler_cut = self
            .handlers
            .iter()
            .position(|handler| handler.finally_depth > level)
            .unwrap_or(self.handlers.len());
        let target_cut = self
            .targets
            .iter()
            .position(|target| target.finally_depth > level)
            .unwrap_or(self.targets.len());
        // One consequence, deliberately not tested: a `break`/`continue` in a finalizer
        // naming a label *inside* the guarded body now finds no target, and `jump` returns
        // without an edge — a copy block with no successors. That jump is a `SyntaxError`
        // and no real parse reaches here with one; of the two wrong answers available for a
        // program that cannot exist, the one that invents no edge is the right one to have.
        //
        // The levels moved out carry their own memos back in with them.
        let inner_finallys = self.finallys.split_off(level);
        let inner_handlers = self.handlers.split_off(handler_cut);
        let inner_targets = self.targets.split_off(target_cut);
        if let Some(tail) = self.statement(body, entry) {
            self.cfg.edge(tail, continuation, EdgeKind::Normal, false);
        }
        self.finallys.extend(inner_finallys);
        self.handlers.extend(inner_handlers);
        self.targets.extend(inner_targets);
        entry
    }

    /// `try`/`catch`/`finally`, with the finalizer emitted once per distinct continuation.
    ///
    /// #192's design, §4.8. Every `return` in the guarded region shares one copy, because
    /// they all continue to the exit; normal completion gets its own — one each for the
    /// body and the `catch` when both complete, per the comment on the loop that emits
    /// them; a propagating `throw` another; each `break`/`continue` target that escapes
    /// one more. After
    /// construction, "the finalizer is on every path out of the `try`" is a fact about the
    /// edge set — there is no flag to read and no keyword to match, which is what #192
    /// asks for.
    ///
    /// The rejected alternative was one shared copy whose exit lists every continuation as
    /// a successor. It invents paths that cannot happen — enter by normal completion,
    /// leave by the `return` continuation — and an invented path is a false positive in a
    /// must-analysis.
    ///
    /// Like [`Self::loop_statement`], this can return `Some(after)` for an `after` that
    /// nothing reaches: `try { a(); } finally { return 1; }` completes normally as far as
    /// the body is concerned, and the finalizer's own `return` is what cuts the edge.
    /// Reachability is a fact about `after`'s predecessor count, here as there.
    fn try_statement(&mut self, node: Node<'t>, current: BlockId) -> Option<BlockId> {
        let body = node.child_by_field_name("body")?;
        let handler = node.child_by_field_name("handler");
        let finalizer = node
            .child_by_field_name("finalizer")
            .and_then(|clause| clause.child_by_field_name("body"));

        // Same reasoning as `if_statement`: without this, `block_of(try_statement)`
        // answers `None`, since the node's own start byte (the `try` keyword) precedes
        // everything attributed inside it. `current` rather than a block of its own — a
        // `try` evaluates nothing before its body, so control is still where it was.
        self.cfg.attribute(current, node);

        let after = self.cfg.alloc(node.end_byte());

        // Order is load-bearing. The finalizer is pushed *before* the handler, so the
        // handler records a depth above it: a `throw` in the body then reaches the
        // `catch` without running the finalizer, which is correct — `finally` runs after
        // `catch`, not instead of it. Pushing them the other way round would run the
        // finalizer twice on that path.
        if let Some(finalizer) = finalizer {
            self.finallys.push(Pending {
                body: finalizer,
                memo: Vec::new(),
            });
        }

        let catch_entry = handler.map(|clause| self.cfg.alloc(clause.start_byte()));
        if let Some(catch_entry) = catch_entry {
            self.handlers.push(Handler {
                to: catch_entry,
                finally_depth: self.finallys.len(),
            });
        }

        let body_entry = self.cfg.alloc(body.start_byte());
        self.cfg.edge(current, body_entry, EdgeKind::Normal, false);
        let body_tail = self.statement(body, body_entry);

        if catch_entry.is_some() {
            self.handlers.pop();
        }

        // The catch runs with the finalizer still pending, so a `throw` inside it unwinds
        // that finalizer on its way to the outer handler.
        let catch_tail = match (handler, catch_entry) {
            (Some(clause), Some(entry)) => match clause.child_by_field_name("body") {
                Some(catch_body) => self.statement(catch_body, entry),
                None => Some(entry),
            },
            _ => None,
        };

        if finalizer.is_some() {
            self.finallys.pop();
        }

        // Normal completion of the body and of the catch both continue to `after`,
        // through their own copy of the finalizer. Their own, because the `Pending` — and
        // with it the memo — does not survive the pop: one copy more than the minimum,
        // left as is rather than hoisting the `Pending` out of the loop, since the count
        // that matters is several `return`s sharing one copy, and that one is exact.
        let mut reachable = false;
        for tail in [body_tail, catch_tail].into_iter().flatten() {
            let target = match finalizer {
                Some(finalizer_body) => {
                    self.finallys.push(Pending {
                        body: finalizer_body,
                        memo: Vec::new(),
                    });
                    let depth = self.finallys.len() - 1;
                    let entry = self.unwind(depth, after);
                    self.finallys.pop();
                    entry
                }
                None => after,
            };
            self.cfg.edge(tail, target, EdgeKind::Normal, false);
            reachable = true;
        }

        reachable.then_some(after)
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
    fn a_root_that_runs_past_the_source_is_refused() {
        // The two parameters are paired by the caller and nothing checks the pairing, so a
        // `source` that is not what `root` was parsed from reaches `text`, which slices
        // `source[node.byte_range()]`. The fixture uses `a && b` on purpose: `binary_expression`
        // is the first arm to read an operator's text, so without the guard this panics inside
        // the walk — measured: `start byte index 17 is out of bounds for string of length 3`,
        // from `text`, naming neither parameter — rather than answering.
        let long = "function f() { a && b; }";
        let tree = parse(long);
        let function = find(&tree, "function_declaration");
        assert!(Cfg::build("f()", function).is_none());
        // And the honest pairing still builds, so the guard is not refusing everything.
        assert!(Cfg::build(long, function).is_some());
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
        // Direction, not membership: asserting only that a True and a False edge exist
        // passes even with the consequence and the join swapped between them — the
        // dominant defect on this branch, found five times across three tasks. Pin which
        // target each label actually leads to instead.
        let source = "function f() { if (c) { a(); } b(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let test = cfg.block_of(find(&tree, "if_statement")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let then_body = cfg.block_of(statements[0]).unwrap();
        let after = cfg.block_of(statements[1]).unwrap();
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
            vec![then_body],
            "the condition being true must enter the consequence"
        );
        assert_eq!(
            to(EdgeKind::False),
            vec![after],
            "the condition being false must reach the join, not the consequence"
        );
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
            assert_eq!(
                attributed(&cfg, arguments),
                vec!["arguments"],
                "{source}: the arguments belong to the chain's continuation block",
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
                "property_identifier"
            ],
            "`c` is read in the one block the chain continues into, past the `?.`",
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
        // The worse half of the same defect: modeled per-link, `a?.b.c()` makes the call
        // happen unconditionally.
        let source = "function f() { const x = a?.b.c(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let arguments = find(&tree, "arguments");
        assert_eq!(
            attributed(&cfg, arguments),
            vec![
                "property_identifier",
                "member_expression",
                "property_identifier",
                "member_expression",
                "arguments",
            ],
            "the call's arguments belong to the chain's continuation block",
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
            vec!["property_identifier"],
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
    fn a_chain_completes_at_its_join() {
        // "Where does this expression complete?" has to have one answer, and for a chain
        // it is the join — the merge point both the short-circuited and the completed
        // path reach. `??` attributes its left operand to the block that operand's
        // evaluation returned, so attributing the outermost link where its *own*
        // evaluation ended would put the chain in two blocks at once.
        let source = "function f() { const x = a?.b ?? c; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let chain = find(&tree, "member_expression");
        let property = chain.child_by_field_name("property").unwrap();

        let rest = cfg.block_of(property).unwrap();
        let join = cfg
            .block(rest)
            .successors
            .iter()
            .find(|e| e.kind == EdgeKind::Normal)
            .map(|e| e.target)
            .expect("the rest of the chain rejoins");
        // Every *occurrence*, not every block: `??` attributes the same chain to this
        // same join as its left operand, so this pins `attribute`'s duplicate guard too.
        let holding: Vec<BlockId> = cfg
            .blocks()
            .flat_map(|(id, block)| {
                block
                    .nodes
                    .iter()
                    .filter(|n| n.id() == chain.id())
                    .map(move |_| id)
            })
            .collect();
        assert_eq!(
            holding,
            vec![join],
            "`a?.b` is attributed to its join, once, and to no other block",
        );
        // And this is the question a rule asks: where does this expression complete?
        // `block_of` can answer it only because an exact match outranks containment — a
        // chain starts where its base starts, so containment alone would answer with the
        // base's block, which sits on one branch of the join.
        assert_eq!(
            cfg.block_of(chain),
            Some(join),
            "a chain resolves to the block it completes in",
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

    fn back_edges(cfg: &Cfg<'_>) -> Vec<(usize, usize)> {
        cfg.blocks()
            .flat_map(|(id, b)| {
                b.successors
                    .iter()
                    .filter(|e| e.back)
                    .map(move |e| (id.index(), e.target.index()))
            })
            .collect()
    }

    #[test]
    fn a_while_loop_has_a_back_edge_to_its_header() {
        let source = "function f() { while (c) { a(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let edges = back_edges(&cfg);
        assert_eq!(edges.len(), 1, "exactly one back edge, got {edges:?}");
        let (from, to) = edges[0];
        assert!(
            to < from,
            "a back edge must target an earlier block, got {from} -> {to}"
        );
    }

    #[test]
    fn a_do_while_loop_has_a_back_edge_from_its_condition() {
        let source = "function f() { do { a(); } while (c); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        assert_eq!(back_edges(&cfg).len(), 1);
    }

    #[test]
    fn every_loop_form_produces_exactly_one_back_edge() {
        for source in [
            "function f() { while (c) { a(); } }",
            "function f() { do { a(); } while (c); }",
            "function f() { for (let i = 0; i < n; i++) { a(); } }",
            "function f() { for (const k in o) { a(); } }",
            "function f() { for (const v of o) { a(); } }",
        ] {
            let tree = parse(source);
            let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
            assert_eq!(back_edges(&cfg).len(), 1, "in `{source}`");
        }
    }

    #[test]
    fn for_of_and_for_in_are_the_same_node_kind() {
        // Guards the design's claim rather than trusting it: one construction covers both,
        // so a change that special-cases `in` would break `of` silently.
        for source in [
            "function f() { for (const k in o) { a(); } }",
            "function f() { for (const v of o) { a(); } }",
        ] {
            let tree = parse(source);
            assert_eq!(find(&tree, "for_in_statement").kind(), "for_in_statement");
            let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
            // The attributed kind, not `.is_some()`: a too-wide attribution still contains
            // the node's start byte and answers `Some` against a broken construction.
            assert!(
                attributed(&cfg, find(&tree, "expression_statement"))
                    .contains(&"expression_statement"),
                "in `{source}`",
            );
        }
    }

    #[test]
    fn a_conditionless_for_loop_has_no_way_to_fall_through() {
        // `condition` and `initializer` are declared `required` in `for_statement`'s
        // grammar, so an omitted clause is not an absent field — `child_by_field_name`
        // returns a present `empty_statement` placeholder instead of `None`. Measured
        // against `tree-sitter-typescript` 0.23.2: `for (;;)` gives
        // `condition = Some(empty_statement)`. Trusting `.is_some()` there would make
        // `for (;;)` read as conditional and invent a `False` edge out of the header,
        // letting `after()` look reachable without ever running the loop body.
        let source = "function f() { for (;;) { a(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let body_call = find(&tree, "call_expression");
        assert!(
            !skips(&cfg, cfg.entry(), cfg.exit(), body_call),
            "an unconditional `for` must not be skippable",
        );
    }

    #[test]
    fn continue_in_a_for_targets_the_increment_not_the_header() {
        let source = "function f() { for (let i = 0; i < n; i++) { continue; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let increment = cfg.block_of(find(&tree, "update_expression")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![increment],
            "continue must reach the increment"
        );
    }

    #[test]
    fn break_leaves_the_loop() {
        let source = "function f() { while (c) { break; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let after = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        // Exactly one edge out, and it leaves the loop rather than continuing it.
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 1, "a break has one way out");
        assert!(
            skips(&cfg, jump, after, find(&tree, "while_statement")),
            "a break must reach the code after the loop without re-entering it",
        );
    }

    #[test]
    fn a_labeled_break_leaves_the_outer_loop() {
        let source = "\
function f() {
  outer: while (a) {
    while (b) {
      break outer;
    }
    inner();
  }
  after();
}";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let inner = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        // `inner()` is after the inner loop and inside the outer one. A labeled break must
        // not reach it; an unlabeled one would.
        assert!(!skips(
            &cfg,
            jump,
            inner,
            find(&tree, "function_declaration")
        ));
    }

    #[test]
    fn a_labeled_continue_targets_the_outer_header() {
        let source = "\
function f() {
  outer: while (a) {
    while (b) {
      continue outer;
    }
  }
}";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 1);
        assert!(targets[0] < jump, "must jump backwards to the outer header");
    }

    #[test]
    fn a_label_on_a_block_answers_break_but_not_continue() {
        let source = "function f() { done: { if (c) { break done; } a(); } b(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        assert!(cfg.block_of(find(&tree, "break_statement")).is_some());
        assert!(back_edges(&cfg).is_empty(), "a labeled block is not a loop");
        // `block_of(...).is_some()` above cannot see a missing edge: `jump` attributes the
        // `break` node to its block unconditionally, before `jump_target` is ever
        // consulted, so presence is guaranteed whether or not an edge was ever added. If
        // `labeled_statement` failed to push a `Target` for this label at all (treating
        // every body as a loop, say), `jump_target` would find nothing and `jump` would
        // add no edge — silently. Assert the edge itself.
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 1, "the break must have exactly one way out");
    }

    #[test]
    fn an_unlabeled_continue_passes_through_a_labeled_block_to_the_loop() {
        // `continue` targets the innermost enclosing *loop*; a labeled block is not a
        // loop, so its `Target` (`continue_to: None`) must not intercept an unlabeled
        // `continue` even though it is nearer on the stack. `block_of(...).is_some()`
        // alone can't see this: `jump` attributes the node before consulting the target
        // stack, so presence is guaranteed regardless of which target — or none — the
        // search actually resolves to. `block_of(while_statement)` resolving at all is
        // itself new: it depends on `while_statement` attributing its own node to the
        // header, the same fix that made `break_leaves_the_loop`'s `skips` exclusion real.
        let source = "function f() { while (c) { done: { continue; } } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let header = cfg.block_of(find(&tree, "while_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![header],
            "continue must reach the loop header, not done's break-only target",
        );
    }

    #[test]
    fn a_continue_through_stacked_labels_reaches_the_loop_header() {
        // `a: b: while (c) {}` is valid JavaScript with both labels naming the same loop.
        // `a`'s body is `b`'s `labeled_statement`, not the `while` directly, so this pins
        // the recursive accumulation in `pending_labels` rather than only the single-label
        // case every other labeled-loop test here exercises.
        let source = "function f() { a: b: while (c) { continue a; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let header = cfg.block_of(find(&tree, "while_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![header],
            "continue a; must reach the loop header through both stacked labels",
        );
    }

    #[test]
    fn a_break_through_stacked_labels_leaves_the_loop() {
        let source = "function f() { a: b: while (c) { break b; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let after = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![after],
            "break b; must leave the loop through both stacked labels",
        );
    }

    #[test]
    fn a_simple_while_condition_labels_true_toward_the_body() {
        // Task 4 hardened only `while (a && b)`-style fixtures, where the condition's own
        // split forces `header != test`. A plain, non-compound condition — where
        // `header == test`, the ordinary case in real code — had no test asserting which
        // way its True/False edges point at all, compound or not.
        let source = "function f() { while (c) { a(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let header = cfg.block_of(find(&tree, "while_statement")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let body_entry = cfg.block_of(statements[0]).unwrap();
        let after = cfg.block_of(statements[1]).unwrap();
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(header)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::True),
            vec![body_entry],
            "a plain while's True edge must enter the body"
        );
        assert_eq!(
            to(EdgeKind::False),
            vec![after],
            "a plain while's False edge must reach the code after the loop"
        );
    }

    #[test]
    fn a_compound_while_condition_puts_the_loops_edges_on_the_join_not_the_header() {
        // `while (a && b)`'s header evaluates only `a`; `split` emits `a`'s own True (to
        // `b`'s block) and False (to the join) *from* `header`. The loop's own True/False
        // — body versus `after` — must come from that join instead, where the *whole*
        // condition's evaluation completes. Wiring them from `header` too would give it
        // two edges per label, indistinguishable from each other.
        //
        // Direction, not membership: collecting kinds and targets into two separate,
        // unpaired vectors and asserting `.contains` on each — as an earlier draft of this
        // test did — passes even if the loop's own True/False are swapped at the join (a
        // loop that runs while its condition is *false*), since `join_kinds` is still
        // `[True, False]` in some order and `header_targets.contains(&join)` is unaffected
        // either way. `and_labels_true_toward_the_right_operand`, above, carries the same
        // lesson for `split`'s own edges.
        let source = "function f() { while (a && b) { break; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let header = cfg.block_of(ident("a")).unwrap();
        // `while_statement`'s own node is attributed to the join (Task 4's own-node
        // attribution fix, combined with this fix's routing of the loop's edges there).
        let join = cfg.block_of(find(&tree, "while_statement")).unwrap();
        let body_entry = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let after = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        assert_ne!(
            header, join,
            "a compound condition's header and join must differ"
        );

        let header_kinds: Vec<EdgeKind> = cfg
            .block(header)
            .successors
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            header_kinds.len(),
            2,
            "header must carry only the condition's own edges, got {header_kinds:?}",
        );

        let to = |from: BlockId, kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(from)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(header, EdgeKind::False),
            vec![join],
            "header's False edge must reach the join"
        );
        assert_eq!(
            to(join, EdgeKind::True),
            vec![body_entry],
            "the join's True edge must enter the body"
        );
        assert_eq!(
            to(join, EdgeKind::False),
            vec![after],
            "the join's False edge must reach the code after the loop"
        );
    }

    #[test]
    fn break_leaves_a_compound_condition_loop_too() {
        // Same property as `break_leaves_the_loop`, on a condition that branches. This is
        // what actually confirms the fix rather than only the block-of-while-statement
        // attribution: before it, `header`'s own False edge (from `split`) bypassed a ban
        // on the join, so this exact mutation was undetectable for any compound-condition
        // loop even after `while_statement` started attributing its own node to the join.
        let source = "function f() { while (a && b) { break; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let after = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 1, "a break has one way out");
        assert!(
            skips(&cfg, jump, after, find(&tree, "while_statement")),
            "a break must reach the code after a compound-condition loop without re-entering it",
        );
    }

    #[test]
    fn a_compound_do_while_condition_puts_the_back_edge_on_the_join_not_the_latch() {
        // Same shape as the compound `while` case: `x && y`'s own `split` emits from
        // `latch` (where the condition starts), so the loop's own True (back to the body)
        // and False (to `after`) must come from the join instead, or `latch` would carry
        // a second, conflicting pair of edges from the condition's own internal branch.
        //
        // Direction, not membership, on its fourth appearance in this task: the original
        // version of this test asserted only `back_edges()[0].0 == join`, the edge's
        // *source*. A mutation swapping the join's two edge targets — the loop runs while
        // its condition is *false* and exits when it is *true* — leaves that assertion
        // unchanged, since the back edge (`back: true`) still leaves the join; only its
        // destination is wrong. Checked directly below instead, with the same
        // `to(from, kind)` idiom the rewritten compound-`while` test above uses.
        let source = "function f() { do { a(); } while (x && y); after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let latch = cfg.block_of(ident("x")).unwrap();
        let join = cfg.block_of(find(&tree, "do_statement")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let body_entry = cfg.block_of(statements[0]).unwrap();
        let after = cfg.block_of(statements[1]).unwrap();
        assert_ne!(
            latch, join,
            "a compound condition's latch and join must differ"
        );

        let latch_kinds: Vec<EdgeKind> =
            cfg.block(latch).successors.iter().map(|e| e.kind).collect();
        assert_eq!(
            latch_kinds.len(),
            2,
            "latch must carry only the condition's own edges, got {latch_kinds:?}",
        );

        let edges = back_edges(&cfg);
        assert_eq!(edges.len(), 1, "exactly one back edge, got {edges:?}");
        assert_eq!(
            edges[0].0,
            join.index(),
            "the back edge must leave the join, not the latch"
        );

        let to = |from: BlockId, kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(from)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(join, EdgeKind::True),
            vec![body_entry],
            "the back edge must target the body entry, not the code after the loop"
        );
        assert_eq!(
            to(join, EdgeKind::False),
            vec![after],
            "the join's False edge must reach the code after the loop, not the body"
        );
    }

    #[test]
    fn a_compound_for_condition_puts_the_loops_edges_on_the_join_not_the_header() {
        // Same property as the `while` case, for `for`'s own condition. Every other `for`
        // fixture in this file has a non-branching condition, so `test == header` there
        // trivially — a wiring bug specific to `for_statement` (swapping `test` and
        // `increment_entry` at the `loop_statement` call site, or passing `header` where
        // `test` belongs) would pass every one of them, and every other test in the file,
        // silently.
        let source = "function f() { for (let i = 0; a && b; i++) { c(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let header = cfg.block_of(ident("a")).unwrap();
        let join = cfg.block_of(find(&tree, "for_statement")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let body_entry = cfg.block_of(statements[0]).unwrap();
        let after = cfg.block_of(statements[1]).unwrap();
        assert_ne!(
            header, join,
            "a compound condition's header and join must differ"
        );

        let header_kinds: Vec<EdgeKind> = cfg
            .block(header)
            .successors
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            header_kinds.len(),
            2,
            "header must carry only the condition's own edges, got {header_kinds:?}",
        );

        let to = |from: BlockId, kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(from)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(header, EdgeKind::False),
            vec![join],
            "header's False edge must reach the join"
        );
        assert_eq!(
            to(join, EdgeKind::True),
            vec![body_entry],
            "the join's True edge must enter the body"
        );
        assert_eq!(
            to(join, EdgeKind::False),
            vec![after],
            "the join's False edge must reach the code after the loop"
        );
    }

    #[test]
    fn a_continue_in_a_compound_while_condition_targets_the_header_not_the_join() {
        // The continue target for a `while` is the header — re-testing the *whole*
        // condition from its start — not the join where the loop's own True/False live,
        // and not `b`'s block either. Targeting the join would skip the condition test
        // entirely: a silent infinite loop, not a visible wrong answer, which is why this
        // needs its own check rather than trusting the "continue re-tests the whole
        // condition" claim on faith. `block_of(while_statement)` resolves to the join, not
        // the header, so `header` is found through `a`'s own block instead.
        let source = "function f() { while (a && b) { continue; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let header = cfg.block_of(ident("a")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![header],
            "continue must target the header, not the join or b's block"
        );
    }

    #[test]
    fn switch_cases_fall_through_to_the_next_body() {
        let source = "function f(x) { switch (x) { case 1: a(); case 2: b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let first = cfg.block_of(statements[0]).unwrap();
        let second = cfg.block_of(statements[1]).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(first)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert!(
            targets.contains(&second),
            "case 1 must fall through to case 2"
        );
    }

    #[test]
    fn a_break_cuts_the_fallthrough() {
        let source = "function f(x) { switch (x) { case 1: a(); break; case 2: b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let first = cfg.block_of(statements[0]).unwrap();
        let second = cfg.block_of(statements[1]).unwrap();
        assert!(
            !skips(&cfg, first, second, find(&tree, "function_declaration")),
            "a break must remove the fallthrough edge",
        );
    }

    #[test]
    fn case_tests_chain_by_the_false_edge() {
        // Direction, not membership: `kinds.contains(&EdgeKind::True) &&
        // kinds.contains(&EdgeKind::False)` would also pass with the match and the
        // chain-onward edges swapped — the same dominant defect flagged above for `if`.
        // Pin which target each edge actually leads to.
        let source = "function f(x) { switch (x) { case 1: a(); break; case 2: b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let cases = find_all(&tree, "switch_case");
        let first_test = cfg
            .block_of(cases[0].child_by_field_name("value").unwrap())
            .unwrap();
        let second_test = cfg
            .block_of(cases[1].child_by_field_name("value").unwrap())
            .unwrap();
        let first_body = cfg
            .block_of(find_all(&tree, "expression_statement")[0])
            .unwrap();
        let to = |kind: EdgeKind| -> Vec<BlockId> {
            cfg.block(first_test)
                .successors
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target)
                .collect()
        };
        assert_eq!(
            to(EdgeKind::True),
            vec![first_body],
            "case 1's test must enter its own body when it matches"
        );
        assert_eq!(
            to(EdgeKind::False),
            vec![second_test],
            "case 1's test must chain to case 2's test when it doesn't match"
        );
    }

    #[test]
    fn a_default_in_a_non_final_position_is_the_last_tests_false_edge() {
        let source = "function f(x) { switch (x) { case 1: a(); default: d(); case 2: b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let cases = find_all(&tree, "switch_case");
        let last_test = cfg
            .block_of(cases[1].child_by_field_name("value").unwrap())
            .unwrap();
        let default_body = cfg
            .block_of(find(&tree, "switch_default").named_child(0).unwrap())
            .unwrap();
        let falsy: Vec<BlockId> = cfg
            .block(last_test)
            .successors
            .iter()
            .filter(|e| e.kind == EdgeKind::False)
            .map(|e| e.target)
            .collect();
        assert_eq!(
            falsy,
            vec![default_body],
            "the last failing test must reach `default`"
        );
    }

    #[test]
    fn a_default_stays_in_the_fallthrough_chain_at_its_own_position() {
        let source = "function f(x) { switch (x) { case 1: a(); default: d(); case 2: b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let statements = find_all(&tree, "expression_statement");
        let (a, d, b) = (
            cfg.block_of(statements[0]).unwrap(),
            cfg.block_of(statements[1]).unwrap(),
            cfg.block_of(statements[2]).unwrap(),
        );
        let out = |id: BlockId| -> Vec<BlockId> {
            cfg.block(id).successors.iter().map(|e| e.target).collect()
        };
        // The whole edge set, not `.contains`: a fallthrough that *also* jumped straight to
        // after-switch would satisfy a membership check while inventing a path out of the
        // middle of the chain. This file's own idiom everywhere else is `assert_eq!` against
        // the full target list, for exactly that reason.
        assert_eq!(out(a), vec![d], "case 1 falls through to default, only");
        assert_eq!(out(d), vec![b], "default falls through to case 2, only");
    }

    #[test]
    fn a_switch_with_no_default_falls_out_when_no_case_matches() {
        // Not `case 1: a();`: that body falls through to after-switch on its own (the
        // ordinary "no next arm" fallthrough), so `after` stays reachable from `test` even
        // with the no-default `False` edge deleted outright — measured by mutation, see
        // the Task 5 fix report. `return` cuts the case body's own fallthrough, which
        // makes the `False` edge the only route from `test` to `after`.
        let source = "function f(x) { switch (x) { case 1: return; } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let after = cfg.block_of(find(&tree, "expression_statement")).unwrap();
        let test = cfg
            .block_of(
                find(&tree, "switch_case")
                    .child_by_field_name("value")
                    .unwrap(),
            )
            .unwrap();
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
            vec![after],
            "with no default, the last failing test must reach the code after the switch"
        );
    }

    #[test]
    fn every_statement_of_a_multi_statement_case_is_reached() {
        // `switch_case`'s `body` is a repeated field. `child_by_field_name` returns the
        // first only, which would silently drop `b()`.
        let source = "function f(x) { switch (x) { case 1: a(); b(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        for statement in find_all(&tree, "expression_statement") {
            // The attributed kind, not `.is_some()`: with `child_by_field_name` the whole
            // `switch_case` would be attributed instead, and its range still contains
            // every statement inside it.
            assert!(
                attributed(&cfg, statement).contains(&"expression_statement"),
                "dropped a case statement: got {:?}",
                attributed(&cfg, statement),
            );
        }
    }

    #[test]
    fn an_empty_switch_falls_through_to_after() {
        // `switch (x) {}`: no arms at all, so `chain_start` and `default_entry` are both
        // `None` and the discriminant must reach after-switch directly.
        let source = "function f(x) { switch (x) {} after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let discriminant = cfg.block_of(find(&tree, "switch_statement")).unwrap();
        let after = cfg.block_of(find(&tree, "expression_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(discriminant)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![after],
            "an empty switch must fall straight through to the code after it",
        );
    }

    #[test]
    fn a_switch_with_only_a_default_reaches_it() {
        // The critical case: zero `switch_case` arms, only `default`. The chain loop
        // `continue`s on every arm (none carries a `value`), so `chain_start` and
        // `previous_test` both stay `None` — every other fixture in this file has at
        // least one preceding `case`, which sets `previous_test` and hides this gap.
        // Without the three-way fallback, the discriminant edges straight to after-switch
        // and `default`'s entry is left with a fallthrough edge out and no edge in:
        // unreachable in the graph while always executing at run time.
        let source = "function f(x) { switch (x) { default: d(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let discriminant = cfg.block_of(find(&tree, "switch_statement")).unwrap();
        let default_body = cfg
            .block_of(find(&tree, "switch_default").named_child(0).unwrap())
            .unwrap();
        // The outgoing edge from the discriminant's own block, and the incoming
        // predecessor on `default`'s entry: asserted directly rather than through
        // `skips`-style reachability, since a redundant path could otherwise mask a
        // missing edge here exactly as it did in the prior fix round.
        let targets: Vec<BlockId> = cfg
            .block(discriminant)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![default_body],
            "with no case tests, the discriminant must reach default's body directly",
        );
        assert!(
            cfg.block(default_body).predecessors.contains(&discriminant),
            "default's entry must have the discriminant's block as a predecessor",
        );
    }

    #[test]
    fn continue_in_a_switch_inside_a_for_targets_the_increment() {
        // A `switch`'s own `Target` must have `continue_to: None`, so an unlabeled
        // `continue` passes through it to the enclosing loop instead of being
        // intercepted by the switch's own after-block. None of this file's other
        // `for`/`continue` fixtures put a `switch` in the loop body, so none of them can
        // catch a switch that wrongly answers `continue` itself.
        let source =
            "function f(x) { for (let i = 0; i < n; i++) { switch (x) { case 1: continue; } } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "continue_statement")).unwrap();
        let increment = cfg.block_of(find(&tree, "update_expression")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(jump)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![increment],
            "continue inside a switch must pass through to the for loop's increment",
        );
    }

    /// Every block attributed a node whose trimmed text is exactly `text`, in source order.
    ///
    /// One entry per copy of a duplicated `finally` — the question about this construction
    /// that `block_of` cannot answer, since several copies hold an equally narrow
    /// attribution of the same node and it answers with the lowest-numbered. A property
    /// that must hold of *every* copy has to be asserted over this, not over `block_of`.
    ///
    /// The text to match is a *statement* — `c();`, not `c()`. `children` attributes
    /// neither a `call_expression` nor its operands, so the narrowest thing attributed
    /// inside a finalizer body is the `expression_statement` around the call.
    fn blocks_with_text(cfg: &Cfg<'_>, source: &str, text: &str) -> Vec<BlockId> {
        cfg.blocks()
            .filter(|(_, b)| {
                b.nodes
                    .iter()
                    .any(|n| source[n.byte_range()].trim() == text)
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// The first block, in source order, attributed a node whose trimmed text is `text`.
    ///
    /// For anything that is *not* copied there is exactly one, and this says which without
    /// going through `block_of`'s containment fallback.
    fn block_with_text(cfg: &Cfg<'_>, source: &str, text: &str) -> BlockId {
        *blocks_with_text(cfg, source, text)
            .first()
            .unwrap_or_else(|| panic!("no block attributed `{text}`"))
    }

    #[test]
    fn a_return_inside_try_runs_the_finally_before_the_exit() {
        let source = "function f() { try { return 1; } finally { c(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        // Deleting the cleanup must disconnect the return from the exit.
        let ret = cfg.block_of(find(&tree, "return_statement")).unwrap();
        assert!(!skips(
            &cfg,
            ret,
            cfg.exit(),
            find(&tree, "call_expression")
        ));
    }

    #[test]
    fn normal_completion_also_runs_the_finally() {
        let source = "function f() { try { a(); } finally { c(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let after = block_with_text(&cfg, source, "after();");
        let body = block_with_text(&cfg, source, "a();");
        let cleanup_call = find_all(&tree, "call_expression")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "c()")
            .unwrap();
        assert!(!skips(&cfg, body, after, cleanup_call));
    }

    #[test]
    fn the_finally_is_copied_once_per_continuation_not_once_per_exit() {
        let source = "\
function f() {
  try {
    if (a) { return 1; }
    if (b) { return 2; }
  } finally { c(); }
}";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let copies = blocks_with_text(&cfg, source, "c();").len();
        // Two returns share one copy (both continue to the exit); normal completion has
        // its own. Never one per `return`.
        assert_eq!(
            copies, 2,
            "expected one copy per continuation, got {copies}"
        );
    }

    #[test]
    fn a_catch_that_falls_through_gets_its_own_copy_of_the_finally() {
        // The commonest `try` shape there is, and no fixture built it: every other
        // `try`/`catch`/`finally` here ends its body in a `throw` or a `return`, so only one
        // of the two tails is ever `Some` and the loop that wires them runs once. With both
        // tails live it runs twice, and each iteration pushes a *fresh* `Pending` — a fresh
        // memo — so normal completion of the body and normal completion of the `catch` get a
        // copy each even though both continue to the same `after`. Two copies for one
        // continuation, which is the documented exception to "once per distinct
        // continuation" and the one place the memo does not do the sharing.
        let source = "function f() { try { a(); } catch (e) { b(); } finally { c(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();

        let copies = blocks_with_text(&cfg, source, "c();");
        assert_eq!(
            copies.len(),
            2,
            "one copy per live tail, not one per continuation, got {}",
            copies.len()
        );

        // Not the count alone: which tail reaches which copy. A construction that emitted two
        // copies but sent both tails into one of them would pass a count assertion while
        // leaving the other copy with no predecessor at all.
        let out = |id: BlockId| -> Vec<BlockId> {
            cfg.block(id).successors.iter().map(|e| e.target).collect()
        };
        let body_tail = block_with_text(&cfg, source, "a();");
        let catch_tail = block_with_text(&cfg, source, "b();");
        assert_eq!(out(body_tail).len(), 1, "the body tail has one successor");
        assert_eq!(out(catch_tail).len(), 1, "the catch tail has one successor");
        assert_ne!(
            out(body_tail),
            out(catch_tail),
            "the two tails must reach different copies; a shared memo would merge them"
        );
        assert!(
            copies.contains(&out(body_tail)[0]) && copies.contains(&out(catch_tail)[0]),
            "each tail must reach one of the counted copies"
        );

        // And both copies converge on the code after the `try`, so the duplication adds
        // blocks without adding a continuation.
        let after = block_with_text(&cfg, source, "after();");
        for &copy in &copies {
            assert_eq!(out(copy), vec![after], "every copy continues to `after`");
        }
    }

    #[test]
    fn two_breaks_to_the_same_target_share_one_finally_copy() {
        // T9-R1: `emit_finally_copy` memoizes on `continuation` alone, so it cannot tell
        // *how* a jump reached that continuation — two `break`s to the same loop share a
        // copy exactly as the two `return`s above share theirs. This is the fixture that
        // was missing: nothing here exercised two same-target jumps out of one `try`.
        let source = "\
function f() {
  while (x) {
    try {
      if (a) break;
      if (b) break;
    } finally { c(); }
  }
}";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();

        let copies = blocks_with_text(&cfg, source, "c();");
        // One copy for both breaks (they share a target: the block after the `while`),
        // one for normal completion of the try body (falling through both `if`s continues
        // the loop instead of leaving it). Never three, which is what one copy per
        // `break` would give.
        assert_eq!(
            copies.len(),
            2,
            "expected one copy per continuation, got {}",
            copies.len()
        );

        // Not just the count: pin down that the two breaks land on the *identical* copy,
        // rather than two different ones that happen to still total two blocks.
        let breaks = find_all(&tree, "break_statement");
        assert_eq!(breaks.len(), 2, "fixture must declare exactly two breaks");
        let successor_of = |node: tree_sitter::Node<'_>| -> BlockId {
            let block = cfg.block_of(node).unwrap();
            let targets: Vec<BlockId> = cfg
                .block(block)
                .successors
                .iter()
                .map(|e| e.target)
                .collect();
            assert_eq!(targets.len(), 1, "a break has exactly one successor");
            targets[0]
        };
        let first = successor_of(breaks[0]);
        let second = successor_of(breaks[1]);
        assert_eq!(
            first, second,
            "both breaks must land on the same finally copy"
        );
        assert!(
            copies.contains(&first),
            "the shared copy must be one of the two counted"
        );
    }

    #[test]
    fn a_throw_reaches_its_catch() {
        // Not `skips(..., find(&tree, "function_declaration"))`: the function root is
        // attributed nowhere, so `block_of` answers `None`, the ban is a no-op, and
        // `skips` degrades to plain reachability — weakening this to "some route exists"
        // rather than pinning the throw's own edge. Assert the successor directly
        // instead, as `a_throw_enters_the_catch_before_the_finally` below already does.
        let source = "function f() { try { throw e; } catch (x) { h(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let thrown = cfg.block_of(find(&tree, "throw_statement")).unwrap();
        let kinds: Vec<EdgeKind> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec![EdgeKind::Exception]);
        let handler = block_with_text(&cfg, source, "h();");
        let targets: Vec<BlockId> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![handler],
            "the throw must reach the catch body"
        );
    }

    #[test]
    fn a_throw_enters_the_catch_before_the_finally() {
        // `finally` runs *after* `catch`, not instead of it, so the throw's own successor
        // is the catch entry and not a copy of the finalizer. The fixture in
        // `a_throw_reaches_its_catch` has no `finally` at all, so it cannot see the
        // difference; asserting the successor rather than reachability is what makes this
        // one see it, since a second copy on the normal-completion path leaves the catch
        // body reachable either way.
        let source = "function f() { try { throw e; } catch (x) { h(); } finally { c(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let thrown = cfg.block_of(find(&tree, "throw_statement")).unwrap();
        let handler = block_with_text(&cfg, source, "h();");
        let targets: Vec<BlockId> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![handler],
            "the throw must enter the catch directly, not through the finalizer",
        );
    }

    #[test]
    fn a_throw_in_catch_reaches_the_function_exit() {
        let source = "function f() { try { a(); } catch (x) { throw x; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        // The attribution is asserted first because the rest of this test passes without
        // it: with no `throw_statement` arm at all, `block_of` falls back to containment,
        // answers with the block holding the whole `try`, and *that* block's one successor
        // is the function's own tail edge to the exit — the right answer for the wrong
        // reason. The kind check is what pins the block to the throw itself.
        assert!(attributed(&cfg, find(&tree, "throw_statement")).contains(&"throw_statement"));
        let thrown = cfg.block_of(find(&tree, "throw_statement")).unwrap();
        let kinds: Vec<EdgeKind> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec![EdgeKind::Exception]);
        let targets: Vec<BlockId> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![cfg.exit()],
            "an uncaught rethrow leaves the function"
        );
    }

    #[test]
    fn a_return_leaves_from_where_its_operand_completes() {
        // The convention `if_statement` states and the catch-all arm follows: a statement
        // is attributed to the block where its own evaluation completes, not to the one
        // control was in when it started. For `return a && b;` the two differ — `a` is
        // evaluated before the split, and the exit edge leaves the join — so attributing
        // the earlier block would answer "which block is this return in?" with one whose
        // successors are the operand's own True/False pair and which has no edge out of
        // the function at all.
        let source = "function f() { return a && b; }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ret = cfg.block_of(find(&tree, "return_statement")).unwrap();
        let targets: Vec<BlockId> = cfg.block(ret).successors.iter().map(|e| e.target).collect();
        assert_eq!(targets, vec![cfg.exit()]);
    }

    #[test]
    fn a_throw_in_a_catch_runs_the_finally_on_its_way_out() {
        // The catch is walked with the finalizer still pending, which is what puts a copy
        // between a rethrow and the exit. `a_throw_in_catch_reaches_the_function_exit`
        // has no `finally`, so popping the finalizer too early is invisible to it.
        // Asserted on the rethrow's own successor: two copies of `c();` exist here — one
        // on this path, one for the body's normal completion — so a `skips` assertion
        // could ban the wrong one and pass regardless.
        let source = "function f() { try { a(); } catch (x) { throw x; } finally { c(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let thrown = cfg.block_of(find(&tree, "throw_statement")).unwrap();
        let targets: Vec<BlockId> = cfg
            .block(thrown)
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 1, "a rethrow has exactly one successor");
        let cleanup = cfg.block(targets[0]);
        assert!(
            cleanup
                .nodes
                .iter()
                .any(|n| source[n.byte_range()].trim() == "c();"),
            "a rethrow must run the finalizer before leaving the function",
        );
        let onward: Vec<BlockId> = cleanup.successors.iter().map(|e| e.target).collect();
        assert_eq!(
            onward,
            vec![cfg.exit()],
            "and that copy continues out of the function"
        );
    }

    #[test]
    fn a_throw_in_a_try_without_a_catch_still_runs_the_finally() {
        let source = "function f() { try { throw e; } finally { c(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let thrown = cfg.block_of(find(&tree, "throw_statement")).unwrap();
        let cleanup = find_all(&tree, "call_expression")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "c()")
            .unwrap();
        assert!(
            !skips(&cfg, thrown, cfg.exit(), cleanup),
            "the finalizer is on every path"
        );
    }

    #[test]
    fn a_return_inside_try_with_no_finally_skips_the_code_after_the_try() {
        let source = "function f() { try { return 1; } catch (x) { h(); } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ret = cfg.block_of(find(&tree, "return_statement")).unwrap();
        let after = block_with_text(&cfg, source, "after();");
        let targets: Vec<BlockId> = cfg.block(ret).successors.iter().map(|e| e.target).collect();
        assert_eq!(targets, vec![cfg.exit()], "a return goes straight out");
        assert!(!skips(
            &cfg,
            ret,
            after,
            find(&tree, "function_declaration")
        ));
    }

    #[test]
    fn a_nested_try_runs_the_inner_finalizer_before_the_outer_one() {
        let source = "\
function f() {
  try {
    try { return 1; } finally { inner(); }
  } finally { outer(); }
}";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let call = |text: &str| {
            find_all(&tree, "call_expression")
                .into_iter()
                .find(|n| &source[n.byte_range()] == text)
                .unwrap()
        };
        let ret = cfg.block_of(find(&tree, "return_statement")).unwrap();
        let inner_block = cfg.block_of(call("inner()")).unwrap();
        // Reaching the outer finalizer from the return must pass through the inner one.
        assert!(!skips(
            &cfg,
            ret,
            cfg.block_of(call("outer()")).unwrap(),
            call("inner()")
        ));
        assert!(skips(&cfg, ret, inner_block, call("outer()")));
    }

    #[test]
    fn a_return_inside_a_finalizer_copy_does_not_re_enter_that_finalizer() {
        // A copy of level N is walked with level N and everything inward split off the
        // pending stack, so a `return` inside it unwinds only what *encloses* the `try`.
        // Without that split the return unwinds this very level again: the memo is keyed
        // on the continuation, and the exit is not the continuation this copy was built
        // for, so a second copy is allocated whose own return then memo-hits into a
        // self-loop — and the function exit stops being reachable from the body at all.
        // `a_nested_try_runs_the_inner_finalizer_before_the_outer_one` cannot see this:
        // neither of its finalizer bodies contains a jump, so nothing inside a copy ever
        // consults the stack the split protects.
        let source = "function f() { try { a(); } finally { return 1; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let copies = blocks_with_text(&cfg, source, "return 1;");
        assert_eq!(copies.len(), 1, "one continuation, one copy");
        // The edge itself, rather than reachability past a node that resolves to no block
        // and therefore bans nothing: under the mutation the copy's `return` lands on a
        // second copy of itself, and the exit is not reachable from either.
        let targets: Vec<BlockId> = cfg
            .block(copies[0])
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![cfg.exit()],
            "the finalizer's own return must leave the function",
        );
    }

    #[test]
    fn only_an_explicit_throw_gets_an_exception_edge() {
        // A call may throw and deliberately gets no edge (#192's design, §4.8). Widening
        // this is #195's decision, not a silent one.
        let source = "function f() { try { risky(); } catch (x) { h(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let exceptions = cfg
            .blocks()
            .flat_map(|(_, b)| b.successors.iter())
            .filter(|e| e.kind == EdgeKind::Exception)
            .count();
        assert_eq!(exceptions, 0, "a call must not get an exception edge in v1");
    }

    #[test]
    fn a_break_out_of_a_try_runs_the_finally() {
        let source = "function f() { while (c) { try { break; } finally { z(); } } after(); }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let jump = cfg.block_of(find(&tree, "break_statement")).unwrap();
        let cleanup = find_all(&tree, "call_expression")
            .into_iter()
            .find(|n| &source[n.byte_range()] == "z()")
            .unwrap();
        let after = block_with_text(&cfg, source, "after();");
        assert!(!skips(&cfg, jump, after, cleanup));
    }

    #[test]
    fn a_try_statement_is_attributed_to_the_block_it_starts_in() {
        // Same reasoning as every other construct here: without an attribution of its own,
        // `block_of(try_statement)` answers with whatever wider node happens to contain
        // the `try` keyword — or `None`. Asserting the attributed *kind* rather than
        // `is_some()` is what distinguishes the two, since `block_of`'s containment
        // fallback answers `Some` for a too-wide attribution as readily as for the right
        // one.
        let source = "function f() { before(); try { a(); } finally { c(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let node = find(&tree, "try_statement");
        assert!(attributed(&cfg, node).contains(&"try_statement"));
        assert_eq!(
            cfg.block_of(node),
            Some(block_with_text(&cfg, source, "before();")),
            "the try belongs to the block control was in when it started",
        );
    }

    #[test]
    fn a_throw_in_a_finally_is_not_caught_by_its_own_try() {
        // A finalizer copy is emitted *during* the guarded body's walk, so this `try`'s own
        // handler is still pushed while the copy is built. It is not in scope for it: a
        // `throw` in a `finally` propagates past the whole statement and is never caught by
        // the `catch` beside it. Asserted over every copy, because `block_of` answers with
        // only the lowest-numbered one and the copies here differ.
        let source = "function f() { try { return 1; } catch (e) { h(); } finally { throw x; } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let handler = block_with_text(&cfg, source, "h();");
        assert_ne!(
            handler,
            cfg.exit(),
            "the catch body must be a block of its own, or the assertion below is vacuous",
        );
        let copies = blocks_with_text(&cfg, source, "throw x;");
        assert_eq!(
            copies.len(),
            2,
            "one copy for the return's continuation, one for the catch's normal completion",
        );
        for copy in copies {
            let targets: Vec<BlockId> = cfg
                .block(copy)
                .successors
                .iter()
                .map(|e| e.target)
                .collect();
            assert_eq!(
                targets,
                vec![cfg.exit()],
                "a throw in a finally leaves the function; it must not reach {handler:?}",
            );
        }
    }

    #[test]
    fn a_continue_in_a_finally_targets_the_loop_the_try_is_in() {
        // The same displaced stack, one construct over: the copy is built inside the *inner*
        // loop's body walk, so that loop's `Target` is still pushed and answers a `continue`
        // the finalizer is not inside. `finally_depth` is what separates the two — the inner
        // loop recorded a depth above this finalizer's level, the outer one below it.
        let source =
            "function f() { while (d) { try { while (c) { return 1; } } finally { continue; } } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let ident = |name: &str| {
            find_all(&tree, "identifier")
                .into_iter()
                .find(|n| &source[n.byte_range()] == name)
                .unwrap()
        };
        let outer_header = cfg.block_of(ident("d")).unwrap();
        let inner_header = cfg.block_of(ident("c")).unwrap();
        assert_ne!(
            outer_header, inner_header,
            "the two loop headers must be distinct blocks, or this asserts nothing",
        );
        let copies = blocks_with_text(&cfg, source, "continue;");
        assert_eq!(
            copies.len(),
            2,
            "one copy for the return, one for normal completion"
        );
        for copy in copies {
            let targets: Vec<BlockId> = cfg
                .block(copy)
                .successors
                .iter()
                .map(|e| e.target)
                .collect();
            assert_eq!(
                targets,
                vec![outer_header],
                "a continue in a finally targets the loop the try is in, not one inside it",
            );
        }
    }

    #[test]
    fn a_comment_before_the_operand_does_not_displace_it() {
        // `comment` is a *named* node in this grammar, so `named_child(0)` hands back the
        // comment and the real operand goes unwalked — the `||` split silently absent, which
        // only ever *removes* paths and so has no symptom. Every other arm reaches its
        // operand through `child_by_field_name`, which cannot pick a comment up; `return`
        // and `throw` declare no fields and have to filter.
        for source in [
            "function f() { return /* why */ a || b; }",
            "function f() { throw /* why */ a || b; }",
        ] {
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
            // `||` takes `False` to its right operand: direction, not membership.
            let false_edges: Vec<BlockId> = cfg
                .block(test)
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::False)
                .map(|e| e.target)
                .collect();
            assert_eq!(
                false_edges,
                vec![right],
                "`{source}`: the operand after the comment was not walked",
            );
        }
    }

    #[test]
    fn a_throw_in_a_finally_reaches_a_handler_outside_the_try() {
        // The *boundary* of the `handlers` cut, which the test above cannot see: there, the
        // only live handler is the finalizer's own `try`, and both `>` and `>=` remove it.
        // Here the enclosing `try`'s handler was recorded at depth 0 — the same level the
        // inner finalizer sits at — so `>` keeps it and `>=` would cut it, leaving the
        // throw with no handler and an edge to the exit instead of into the catch.
        let source =
            "function f() { try { try { return 1; } finally { throw x; } } catch (e) { h(); } }";
        let tree = parse(source);
        let cfg = Cfg::build(source, find(&tree, "function_declaration")).unwrap();
        let handler = block_with_text(&cfg, source, "h();");
        assert_ne!(
            handler,
            cfg.exit(),
            "the catch body must be a block of its own, or the assertion below is vacuous",
        );
        let copies = blocks_with_text(&cfg, source, "throw x;");
        assert_eq!(
            copies.len(),
            1,
            "the return is the only continuation out of the inner try",
        );
        let targets: Vec<BlockId> = cfg
            .block(copies[0])
            .successors
            .iter()
            .map(|e| e.target)
            .collect();
        assert_eq!(
            targets,
            vec![handler],
            "a throw in a finally is caught by a handler outside the try it belongs to",
        );
    }

    /// A function body of `n` statements, mixing straight-line and branching forms.
    ///
    /// Duplicated verbatim in `benches/cfg.rs`, which is a separate crate target compiled
    /// without `cfg(test)` and so cannot reach this module. Keep the two in step.
    #[expect(
        clippy::format_push_string,
        reason = "builds a one-off test fixture, not a hot path; `allow-*-in-tests` does not \
                  reach this lint the way it does `unwrap`/`expect`"
    )]
    fn synthetic(n: usize) -> String {
        let mut source = String::from("function f(x) {\n");
        for i in 0..n {
            match i % 4 {
                0 => source.push_str(&format!("  const v{i} = x + {i};\n")),
                1 => source.push_str(&format!("  if (v{} > 0) {{ g({i}); }}\n", i - 1)),
                2 => source.push_str(&format!("  while (v{} > {i}) {{ h({i}); }}\n", i - 2)),
                _ => source.push_str(&format!("  const w{i} = a{i} && b{i};\n")),
            }
        }
        source.push_str("  return 0;\n}\n");
        source
    }

    #[test]
    fn blocks_and_edges_stay_linear_in_statement_count() {
        // A wall-clock ceiling is what AGENTS.md records timing out at 120 s under a
        // loaded gate while passing alone in 33 s. Shape is what actually catches the
        // regression that would blow the cold budget, and it cannot flake.
        let mut measurements = Vec::new();
        for n in [100usize, 200, 400] {
            let source = synthetic(n);
            let tree = parse(&source);
            let cfg = Cfg::build(&source, find(&tree, "function_declaration")).unwrap();
            let blocks = cfg.blocks().count();
            let edges: usize = cfg.blocks().map(|(_, b)| b.successors.len()).sum();
            measurements.push((n, blocks, edges));
        }
        // Measured against this fixture with the un-mutated builder: blocks(n) = 1.75n + 3
        // and edges(n) = 2.5n + 2, both exact affine fits (checked at n=100/200/400, not
        // approximated). The multipliers below are chosen against those fits rather than
        // picked round: 3x and 4x leave ~1.6-1.7x headroom over the observed max ratio
        // (1.78, 2.52) — room for `synthetic`'s statement mix to shift a little without
        // flaking, not room to hide a regression that doubles the per-statement cost.
        for &(n, blocks, edges) in &measurements {
            assert!(
                blocks < n * 3,
                "blocks must stay linear: {blocks} for {n} statements ({measurements:?})",
            );
            assert!(
                edges < n * 4,
                "edges must stay linear: {edges} for {n} statements ({measurements:?})",
            );
        }
        // Quadrupling the input must not more than roughly quadruple the graph. The real ratio
        // at 4x is 3.95 against this 5x limit — 26% headroom, which would be uncomfortably
        // tight for a timing assertion but is fine here: construction is deterministic, so
        // the ratio is exact and cannot drift under load. 5x is loose enough to pass
        // genuine linear growth and tight enough to catch what 6x would not: an
        // `O(n log n)` regression measures ~5.2x at this input ratio, under the old limit
        // and over this one.
        let (_, small, _) = measurements[0];
        let (_, large, _) = measurements[2];
        assert!(
            large < small * 5,
            "4x the statements gave {large} blocks against {small} — superlinear",
        );
    }

    #[test]
    fn enclosing_cfg_root_finds_the_nearest_function() {
        let source = "function outer() { function inner() { acquire(); } }";
        let tree = parse(source);
        let acquire = find(&tree, "call_expression");
        let root = super::enclosing_cfg_root(acquire).expect("a root");
        assert_eq!(root.kind(), "function_declaration");
        // nearest, not outermost: it is `inner`, whose body holds the call
        assert!(source[root.byte_range()].starts_with("function inner"));
    }

    #[test]
    fn a_top_level_node_roots_at_the_program() {
        let source = "acquire();";
        let tree = parse(source);
        let acquire = find(&tree, "call_expression");
        assert_eq!(
            super::enclosing_cfg_root(acquire).unwrap().kind(),
            "program"
        );
    }
}
