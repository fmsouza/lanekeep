//! The oracle itself: construction, dispatch, and the bound that makes it terminate.

use std::fmt;
use std::sync::Arc;

use lanekeep_lang::Language;
use lanekeep_lang::binding::BindingResolver;
use tree_sitter::{Node, Tree};

use crate::table;
use crate::types::{Primitive, Symbol, Type};

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
    "identifier",
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
    "binary_expression",
    "unary_expression",
    "call_expression",
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
    tree: &'t Tree,
    source: &'t str,
    resolver: Arc<dyn BindingResolver>,
}

/// Hand-written because `Arc<dyn BindingResolver>` is not `Debug` — the trait answers
/// identifier questions, not requests to describe itself, and requiring every implementor
/// to add one for the sake of this impl is not worth it. The same reasoning, and the same
/// fix, as `LanguageRegistry` in `lanekeep-lang`.
///
/// `tree` has no such problem — `Tree`'s own `Debug` delegates to the root `Node`'s,
/// which prints one line (measured: `{Tree {Node program (0, 0) - (0, 16)}}`) rather than
/// the whole parse tree, so it costs nothing to include.
impl fmt::Debug for TypeScriptOracle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeScriptOracle")
            .field("tree", &self.tree)
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

            "binary_expression" => {
                let next = depth.saturating_add(1);
                let left = self.primitive_of(node.child_by_field_name("left")?, next);
                let right = self.primitive_of(node.child_by_field_name("right")?, next);
                table::binary(self.operator_of(node)?, left, right).map(Type::Primitive)
            }

            "unary_expression" => table::unary(self.operator_of(node)?).map(Type::Primitive),

            "call_expression" => {
                let callee = node.child_by_field_name("function")?;
                // Only a *bare* global counts. A member call like `Number.parseFloat(x)`
                // is not in the table, and a callee that resolves to a local binding is
                // somebody's own function that happens to share a name.
                if callee.kind() != "identifier" {
                    return None;
                }
                if self
                    .resolver
                    .resolve(self.tree, self.source, callee)
                    .is_some()
                {
                    return None;
                }
                table::builtin_call(self.text(callee)).map(Type::Primitive)
            }

            "type_annotation" => {
                self.annotation_type(node.named_child(0)?, depth.saturating_add(1))
            }
            "predefined_type" | "union_type" | "literal_type" | "type_identifier" => {
                self.annotation_type(node, depth)
            }

            "identifier" => {
                let declaration = self.resolver.declaration_of(self.tree, self.source, node)?;
                self.declaration_type(declaration, depth.saturating_add(1))
            }

            _ => None,
        }
    }

    /// The type a declaration gives the name it declares.
    ///
    /// An annotation is preferred over an initializer wherever both are present, because
    /// the annotation is what the program means: `const x: string = parseFloat(s)` is a
    /// type error, and answering `number` for it would describe the mistake rather than the
    /// declaration.
    fn declaration_type(&self, declaration: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let next = depth.saturating_add(1);

        match declaration.kind() {
            "required_parameter" | "optional_parameter" => {
                // The `type` field is the `type_annotation` wrapper; the parameter node
                // itself is not one, so it has to be read before unwrapping. An unannotated
                // parameter has no `type` field and gives nothing, which is correct — this
                // milestone does not infer a parameter's type from its call sites.
                let annotation = declaration.child_by_field_name("type")?;
                self.annotation_type(annotation_child(annotation)?, next)
            }

            "variable_declarator" => {
                if let Some(annotation) = declaration.child_by_field_name("type") {
                    return self.annotation_type(annotation_child(annotation)?, next);
                }
                self.type_of_at(declaration.child_by_field_name("value")?, next)
            }

            // An import's declaration is in another file, which this oracle does not open.
            // A function or class declaration names a callable or a constructor rather than
            // a value with a type this milestone reasons about.
            _ => None,
        }
    }

    /// A node's type, when it is a primitive and nothing else.
    ///
    /// The operator table reasons about primitives, and a nominal or a union on either side
    /// of an arithmetic operator is something it has no row for.
    fn primitive_of(&self, node: Node<'t>, depth: u32) -> Option<Primitive> {
        match self.type_of_at(node, depth)? {
            Type::Primitive(primitive) => Some(primitive),
            Type::Nominal { .. } | Type::Union(_) => None,
        }
    }

    /// The type a type-level node denotes.
    ///
    /// Separate from [`Self::type_of_at`] because the two vocabularies barely overlap: a
    /// `number` in expression position is a literal and in type position is a keyword. One
    /// match arm handling both would have to disambiguate by parent, which is the kind of
    /// thing that is right until somebody nests it.
    fn annotation_type(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return None;
        }

        match node.kind() {
            // Matched on text, not kind: `any` and `unknown` parse identically to `number`.
            // Both give nothing, deliberately — `any` is the absence of a claim, and
            // `void` and `never` are types no rule built on this oracle asks about.
            "predefined_type" => match self.text(node) {
                "number" => Some(Type::Primitive(Primitive::Number)),
                "string" => Some(Type::Primitive(Primitive::String)),
                "boolean" => Some(Type::Primitive(Primitive::Boolean)),
                "bigint" => Some(Type::Primitive(Primitive::BigInt)),
                "symbol" => Some(Type::Primitive(Primitive::Symbol)),
                _ => None,
            },

            "union_type" => {
                let next = depth.saturating_add(1);
                let mut cursor = node.walk();
                let members: Vec<Type> = node
                    .children(&mut cursor)
                    .filter(Node::is_named)
                    .filter_map(|member| self.annotation_type(member, next))
                    .collect();
                // A member the oracle could not type is dropped rather than making the
                // whole union unknown. `number | SomeGeneric<T>` still tells a rule asking
                // "can this be a number" something true.
                Type::union(members)
            }

            // A literal type wraps the literal itself, so the expression side answers it.
            "literal_type" => self.type_of_at(node.named_child(0)?, depth.saturating_add(1)),

            // `bigint` is the one primitive-type keyword this grammar does not lex as a
            // `predefined_type` — verified against tree-sitter-typescript 0.23 with a parse
            // probe: `let x: bigint;` produces a `type_identifier` node reading "bigint",
            // where `number`, `string`, `boolean`, `symbol`, `any` and `unknown` all produce
            // `predefined_type`. Matched on text for the same reason the arm above matches
            // on text rather than kind — but the resolver gets first say: `class bigint {}`
            // shadows the primitive exactly as a local `parseFloat` shadows the builtin
            // conversion in `type_of_at`, so the check has to run before the shortcut, not
            // after `named_type` would have caught it anyway.
            "type_identifier" => {
                if self.text(node) == "bigint"
                    && self
                        .resolver
                        .resolve(self.tree, self.source, node)
                        .is_none()
                {
                    return Some(Type::Primitive(Primitive::BigInt));
                }
                self.named_type(node, depth)
            }

            // Generic, conditional, mapped, function and object types. Each would need an
            // abstraction this oracle does not have, and guessing is worse than silence.
            _ => None,
        }
    }

    /// A type named by an identifier.
    ///
    /// Nominal for now. Task 8 gives this the alias-following arm that makes `depth`
    /// load-bearing; until then it is threaded through and unused, which the underscore
    /// records rather than hides.
    fn named_type(&self, node: Node<'t>, _depth: u32) -> Option<Type> {
        let name = self.text(node);
        if name.is_empty() {
            return None;
        }

        Some(Type::Nominal {
            name: name.to_owned(),
            symbol: self.symbol_at(node),
        })
    }

    /// Where the name at `node` came from, when the resolver can say.
    fn symbol_at(&self, node: Node<'t>) -> Option<Symbol> {
        use lanekeep_lang::binding::Binding;

        let name = self.text(node);
        if name.is_empty() {
            return None;
        }
        let module = match self.resolver.resolve(self.tree, self.source, node)? {
            Binding::Import { module, .. } => Some(module),
            Binding::Local(_) => None,
        };
        Some(Symbol {
            name: name.to_owned(),
            module,
        })
    }

    /// The operator token of a binary or unary expression.
    ///
    /// `operator` is a real field on both node kinds, same as `left`, `right` and
    /// `function` beside it — the token it points to is an anonymous *node* (there is no
    /// dedicated `+` or `typeof` kind), but anonymous-ness is a property of the node, not
    /// of whether a field names it. The two are independent, and it is only the former that
    /// is true here.
    fn operator_of(&self, node: Node<'t>) -> Option<&'t str> {
        node.child_by_field_name("operator")
            .map(|child| self.text(child))
    }

    /// The source text of a node.
    fn text(&self, node: Node<'t>) -> &'t str {
        self.source.get(node.byte_range()).unwrap_or("")
    }
}

/// The type inside a `type_annotation` wrapper.
///
/// A parameter's `type` field is the `type_annotation` node, not the type itself, so every
/// caller reading an annotation has to step through it. One place to get that wrong is
/// better than four.
fn annotation_child(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "type_annotation" {
        node.named_child(0)
    } else {
        Some(node)
    }
}
