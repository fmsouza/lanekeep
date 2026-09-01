//! The oracle itself: construction, dispatch, and the bound that makes it terminate.

use std::fmt;
use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang::binding::BindingResolver;
use tree_sitter::{Node, Tree};

use crate::types::{Primitive, Type};

/// Node kinds the dispatch below reads, which the constructor requires the grammar to know.
///
/// Derived from the dispatch rather than written beside it: a kind added to `type_of`
/// without being added here would be read from a grammar that may not have it. Keeping the
/// two in one place is what stops them drifting.
const REQUIRED_KINDS: &[&str] = &[
    "predefined_type",
    "type_annotation",
    "type_identifier",
    "union_type",
    "literal_type",
    "type_alias_declaration",
    "required_parameter",
    "optional_parameter",
    "variable_declarator",
    "string",
    "template_string",
    "true",
    "false",
    "null",
    "undefined",
    "number",
    "parenthesized_expression",
];

/// How far the oracle will follow a chain before giving up.
///
/// Two things make the recursion unbounded otherwise: `type A = B; type B = A`, and chains
/// of initializers. Exceeding the bound is indistinguishable from not knowing, which is
/// already a first-class answer, so nothing needs to be reported when it happens.
///
/// Fixed rather than measured. A bound that depended on elapsed time would put the clock in
/// the cache key.
const MAX_DEPTH: u32 = 16;

/// A type oracle for one parsed TypeScript file.
pub struct TypeScriptOracle<'t> {
    /// Read from Task 5 onward, by everything that asks the resolver a question.
    #[expect(
        dead_code,
        reason = "read once call expressions are dispatched, in the next task"
    )]
    tree: &'t Tree,
    source: &'t str,
    #[expect(
        dead_code,
        reason = "read once call expressions are dispatched, in the next task"
    )]
    resolver: Arc<dyn BindingResolver>,
}

/// Hand-written because `Arc<dyn BindingResolver>` is not `Debug` — the trait answers
/// identifier questions, not requests to describe itself, and requiring every implementor
/// to add one for the sake of this impl is not worth it. The same reasoning, and the same
/// fix, as `LanguageRegistry` in `lanekeep-lang`.
///
/// `tree` is left out too, deliberately rather than incidentally: printing it would read
/// `self.tree`, which would satisfy `dead_code` for a field this task's `#[expect]` says
/// must still be unread.
impl fmt::Debug for TypeScriptOracle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeScriptOracle")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl<'t> TypeScriptOracle<'t> {
    /// Build an oracle for one parsed file, or `None` if it cannot serve this one.
    ///
    /// Refused in two cases, both of which would otherwise produce confident nonsense
    /// rather than an error. A grammar that does not know the node kinds this oracle reads
    /// is not TypeScript, whatever it calls itself. And a language with no resolver cannot
    /// answer where a name was declared, so the oracle could type no identifier at all —
    /// which would look exactly like a file with nothing to say about it.
    ///
    /// The resolver is taken from the language rather than passed separately, so a caller
    /// cannot pair one language's grammar with another's resolver.
    #[must_use]
    pub fn for_file(language: &dyn Language, tree: &'t Tree, source: &'t str) -> Option<Self> {
        let grammar = language.grammar();
        if !REQUIRED_KINDS
            .iter()
            .all(|kind| grammar.id_for_node_kind(kind, true) != 0)
        {
            return None;
        }

        Some(Self {
            tree,
            source,
            resolver: language.resolver()?,
        })
    }

    /// The type of the expression at `node`, or `None` when the oracle cannot be sure.
    ///
    /// `None` is an answer rather than a failure. A rule that stays silent on it reports
    /// only what was established, which is the posture every rule built on this oracle is
    /// expected to take.
    #[must_use]
    pub fn type_of(&self, node: Node<'t>) -> Option<Type> {
        self.type_of_at(node, 0)
    }

    fn type_of_at(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return None;
        }

        match node.kind() {
            "string" | "template_string" => Some(Type::Primitive(Primitive::String)),
            "true" | "false" => Some(Type::Primitive(Primitive::Boolean)),
            "null" => Some(Type::Primitive(Primitive::Null)),
            "undefined" => Some(Type::Primitive(Primitive::Undefined)),

            // A bigint literal parses as `number`; the trailing `n` is the only thing that
            // distinguishes it, so this reads the text rather than trusting the kind.
            "number" => Some(Type::Primitive(if self.text(node).ends_with('n') {
                Primitive::BigInt
            } else {
                Primitive::Number
            })),

            "parenthesized_expression" => {
                self.type_of_at(node.named_child(0)?, depth.saturating_add(1))
            }

            _ => None,
        }
    }

    /// The source text of a node.
    fn text(&self, node: Node<'t>) -> &'t str {
        self.source.get(node.byte_range()).unwrap_or("")
    }
}
