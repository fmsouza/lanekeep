//! The oracle itself: construction, dispatch, and the bound that makes it terminate.

use std::cell::Cell;
use std::fmt;
use std::sync::Arc;

use lanekeep_core::FilePath;
use lanekeep_lang::Language;
use lanekeep_lang::binding::{Binding, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

use crate::declarations::ExportTarget;
use crate::table;
use crate::types::{Primitive, Symbol, Type};

/// What an oracle asks its host when a name comes from another file.
///
/// A trait rather than a concrete provider, so this crate's layering holds: the oracle reads
/// **one** tree and nothing else, and every question about *which other file* and *how deep*
/// belongs to the value that owns the declaration cache and the budget. An oracle with no
/// implementation attached answers exactly what it answered before cross-file resolution
/// existed, which is what keeps `TypeScriptOracle::new` a within-file oracle.
///
/// Every method takes the *importing* file, because a relative specifier means nothing
/// without one, and a `depth` already spent, because a bound reset at every file boundary is
/// not a bound.
pub trait ImportResolution {
    /// The type an imported *value* has, computed in its declaring file's own context.
    fn imported_value_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Option<Type>;

    /// The type an imported *type alias* names, when the imported name is one.
    ///
    /// Deliberately not "the type of the imported type". An imported class or interface keeps
    /// its own nominal identity and its use-site symbol — replacing it with whatever its
    /// declaration file says would drop the module the name was imported from, which is the
    /// one field `lanekeep/no-restricted-types` matches on. Only an alias is transparent,
    /// exactly as a same-file `type Amount = number` already is.
    ///
    /// Returns [`Followed`] rather than `Option<Type>` because the caller's fallback depends
    /// on *why* there is no type: a name that simply is not an alias keeps its own nominal
    /// identity (as it always has), but a name that *is* an alias whose chain was cut by
    /// `MAX_DEPTH` must not — falling back there would answer with an intermediate file's
    /// own nominal type, a confident guess rather than the honest "unknown" a cut chain
    /// deserves. See `Followed`'s own documentation.
    fn imported_alias_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Followed;

    /// What calling an imported function yields.
    fn imported_return_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Option<Type>;

    /// Where an imported name is actually declared, after every re-export.
    fn imported_export(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
    ) -> Option<ExportTarget>;
}

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
    "type_parameter",
    "identifier",
    "required_parameter",
    "optional_parameter",
    "variable_declarator",
    // Not a type node, and read all the same: a `comment` is a *named* child of a
    // `union_type`, so the union arm has to name it in order to skip it. See there.
    "comment",
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
    // The declaration walk's own vocabulary (`declarations.rs`). A grammar without these
    // cannot answer a cross-file question, and probing for them here is what keeps the
    // provider from opening a file it has no way to read.
    "export_statement",
    "export_clause",
    "export_specifier",
    "namespace_export",
    "ambient_declaration",
    "lexical_declaration",
    "variable_declaration",
    "function_signature",
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
    "enum_declaration",
    "module",
    "internal_module",
    "class_heritage",
    "extends_clause",
    "extends_type_clause",
    "import_statement",
];

/// What following an imported name across the file boundary, as a type alias, found.
///
/// A plain `Option<Type>` cannot tell two failure shapes apart, and `named_type`'s fallback
/// has to answer them differently: "this name is not an alias at all" keeps its own nominal
/// identity, exactly as it always has, while "this name is an alias, but the chain following
/// it was cut by `MAX_DEPTH`" must answer nothing — see addendum B of task 4.16. The
/// distinction has to survive an arbitrary number of cross-file hops, because the bound can
/// be spent several files away from the frame that first asked; every hop threads this enum
/// rather than collapsing it back to `Option` until the walk has fully unwound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The type the alias names.
    Type(Type),
    /// The name is an alias, but `MAX_DEPTH` cut the chain before it resolved to a type.
    Exhausted,
    /// The name does not name a type alias at all.
    NotAnAlias,
}

/// How far the oracle will follow a chain before giving up.
///
/// Two things make the recursion unbounded otherwise: `type A = B; type B = A`, and chains
/// of initializers. Exceeding the bound is indistinguishable from not knowing, which is
/// already a first-class answer, so nothing needs to be reported when it happens.
///
/// Fixed rather than measured. A bound that depended on elapsed time would put the clock in
/// the cache key.
pub(crate) const MAX_DEPTH: u32 = 16;

/// A type oracle for one parsed TypeScript file.
pub struct TypeScriptOracle<'t> {
    tree: &'t Tree,
    source: &'t str,
    resolver: Arc<dyn BindingResolver>,
    /// Which file this parse is of, when the caller could say.
    ///
    /// Required for cross-file resolution and for nothing else, which is why it is optional:
    /// a within-file question does not need to know where the file lives, and demanding one
    /// would make every existing caller supply a value it has no use for.
    file: Option<&'t FilePath>,
    imports: Option<&'t dyn ImportResolution>,
    /// Set the moment this oracle gives up on [`MAX_DEPTH`], when a caller asked to be told.
    ///
    /// The bound answers a bare `None`, which is indistinguishable from "there is no type
    /// here" — and a caller threading a depth it has already spent needs the difference: an
    /// answer the bound truncated describes the *prefix* the caller walked, not the node it
    /// asked about, so it must not be memoized against that node. A `Cell` rather than a
    /// return-type change because the bound is checked in four recursive arms several frames
    /// below any public method, exactly the shape `Imports`' own flag exists for. `None` when
    /// nobody asked, which is every within-file caller.
    exhausted: Option<&'t Cell<bool>>,
}

/// Hand-written because `Arc<dyn BindingResolver>` is not `Debug` — the trait answers
/// identifier questions, not requests to describe itself, and requiring every implementor
/// to add one for the sake of this impl is not worth it. The same reasoning, and the same
/// fix, as `LanguageRegistry` in `lanekeep-lang`.
///
/// `tree` has no such problem — `Tree`'s own `Debug` delegates to the root `Node`'s,
/// which prints one line (measured: `{Tree {Node program (0, 0) - (0, 12)}}`) rather than
/// the whole parse tree, so it costs nothing to include.
///
/// `source` is the one field deliberately summarized rather than printed. It is a whole
/// file, and a `Debug` that puts a file into every line it appears in is not one anybody
/// can read; its length identifies which file this is for as well as the bytes would.
/// Same call as `LanguageRegistry`, which prints its keys and not the languages behind
/// them.
impl fmt::Debug for TypeScriptOracle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeScriptOracle")
            .field("tree", &self.tree)
            .field("source_len", &self.source.len())
            .field("has_imports", &self.imports.is_some())
            .finish_non_exhaustive()
    }
}

/// A grammar confirmed to speak TypeScript, and the resolver that goes with it.
///
/// Separate from the oracle because probing is 8.4 µs of a 9.2 µs construction — 23
/// `id_for_node_kind` calls, each a linear scan over a 383-kind table. Paying that once per
/// run rather than once per query is what keeps the type surface from costing thirty host
/// crossings on every call, against a crossing §15.1 measures at ~302 ns.
#[derive(Clone)]
pub struct TypeScriptSupport {
    resolver: Arc<dyn BindingResolver>,
}

impl fmt::Debug for TypeScriptSupport {
    /// Hand-written because `Arc<dyn BindingResolver>` is not `Debug`, the same reason and
    /// the same shape as `LanguageRegistry`'s.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeScriptSupport").finish_non_exhaustive()
    }
}

impl TypeScriptSupport {
    /// Confirm a grammar has the vocabulary the oracle reads, and take its resolver.
    ///
    /// `None` in two cases, both of which would otherwise produce confident nonsense rather
    /// than an error. A grammar that does not know the node kinds this oracle reads is not
    /// TypeScript, whatever it calls itself. And a language with no resolver cannot say where
    /// a name was declared, so the oracle could type no identifier at all — which would look
    /// exactly like a file with nothing to say about it.
    #[must_use]
    pub fn probe(language: &dyn Language) -> Option<Self> {
        let grammar = language.grammar();
        if !REQUIRED_KINDS
            .iter()
            .all(|kind| grammar.id_for_node_kind(kind, true) != 0)
        {
            return None;
        }
        Some(Self {
            resolver: language.resolver()?,
        })
    }

    /// The resolver the probe took.
    ///
    /// Handed to every [`crate::declarations::Declaration`] this support's provider parses,
    /// and to the walks over the asking file's own tree, so that "which statement declares
    /// this name" is answered by the one resolver the run was probed with — through the
    /// trait, never through a language crate this one would otherwise have to name.
    pub(crate) fn resolver(&self) -> &Arc<dyn BindingResolver> {
        &self.resolver
    }
}

impl<'t> TypeScriptOracle<'t> {
    /// Build an oracle for one parsed file.
    ///
    /// Cheap by construction: everything expensive happened in [`TypeScriptSupport::probe`].
    /// That is what lets a caller build one of these per query rather than per run.
    #[must_use]
    pub fn new(support: &TypeScriptSupport, tree: &'t Tree, source: &'t str) -> Self {
        Self {
            tree,
            source,
            resolver: Arc::clone(&support.resolver),
            file: None,
            imports: None,
            exhausted: None,
        }
    }

    /// Let this oracle follow a name into the file that declares it.
    ///
    /// Without it every arm behaves exactly as it did before cross-file resolution existed —
    /// an import is a name with a module and no type — which is what makes a within-file
    /// oracle still a thing this crate can hand out.
    #[must_use]
    pub fn with_imports(mut self, file: &'t FilePath, imports: &'t dyn ImportResolution) -> Self {
        self.file = Some(file);
        self.imports = Some(imports);
        self
    }

    /// Let this oracle report that its depth bound — rather than the program — is why it
    /// answered nothing.
    ///
    /// For a caller that threads a depth it has already spent and memoizes what comes back.
    /// A bare `None` cannot say this on its own — it is what the bound and an untypeable
    /// node both answer — and the bound is checked several frames below any public method, so
    /// the flag is the channel rather than a return type.
    #[must_use]
    pub fn with_exhaustion(mut self, exhausted: &'t Cell<bool>) -> Self {
        self.exhausted = Some(exhausted);
        self
    }

    /// Answer nothing, and say the bound is why.
    fn exhaust<T>(&self) -> Option<T> {
        if let Some(flag) = self.exhausted {
            flag.set(true);
        }
        None
    }

    /// The type of `node`, starting from a depth already spent.
    ///
    /// For a provider that has followed an import: the recursion crosses files, and a bound
    /// reset at every boundary is not a bound at all.
    #[must_use]
    pub fn type_of_from(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        self.type_of_at(node, depth)
    }

    /// The type a declaration gives the name it declares, from a depth already spent.
    #[must_use]
    pub fn declaration_type_from(&self, declaration: Node<'t>, depth: u32) -> Option<Type> {
        self.declaration_type(declaration, depth)
    }

    /// The return type of `node`, from a depth already spent. See [`Self::type_of_from`].
    #[must_use]
    pub fn return_type_from(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        self.return_type_at(node, depth)
    }

    /// The type a name *in type position* denotes: an alias followed, a nominal otherwise.
    ///
    /// [`Self::type_of`] cannot stand in for it. In expression position an `identifier` is a
    /// value, so `class A extends B {}`'s `B` would be typed as whatever value `B` holds —
    /// which for a class declaration is nothing at all — rather than as the type it names.
    #[must_use]
    pub fn type_named_by(&self, node: Node<'t>) -> Option<Type> {
        self.named_type(node, 0)
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

    /// Where the name at `node` came from, or `None` if nothing in this file declares it.
    ///
    /// Distinct from [`Self::type_of`] and useful where that returns nothing: an imported
    /// value has no type this oracle can read, and still has a name and a module — which is
    /// exactly what a rule distinguishing one library's `Decimal` from a local class needs.
    ///
    /// Answers in type position as well as expression position, because the resolver does.
    #[must_use]
    pub fn symbol_of(&self, node: Node<'t>) -> Option<Symbol> {
        self.symbol_at(node)
    }

    /// What calling the function at `node` yields.
    ///
    /// Separate from [`Self::type_of`] rather than folded into it, and the reason is the
    /// vocabulary rather than the plumbing: a function declaration is not an expression, and
    /// giving `type_of` a signature type would mean a `Type::Function` variant every rule
    /// asking a simpler question would then have to unpack. There is exactly one question
    /// rules ask about a function, so there is exactly one method.
    ///
    /// Accepts a call expression (whose callee is resolved), a function-like declaration, or
    /// an identifier bound to one.
    #[must_use]
    pub fn return_type_of(&self, node: Node<'t>) -> Option<Type> {
        self.return_type_at(node, 0)
    }

    fn return_type_at(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return self.exhaust();
        }
        let next = depth.saturating_add(1);

        match node.kind() {
            "call_expression" => self.return_type_at(node.child_by_field_name("function")?, next),
            "identifier" => {
                if let Some(Binding::Import { module, name }) =
                    self.resolver.resolve(self.tree, self.source, node)
                    && let (Some(file), Some(imports)) = (self.file, self.imports)
                {
                    return imports.imported_return_type(file, &module, &name, next);
                }
                let declaration = self.resolver.declaration_of(self.tree, self.source, node)?;
                self.return_type_at(declaration, next)
            }
            // `const rate = () => 1` binds the function to a name; the declarator's value is
            // the function. An annotated declarator is deliberately not read as a signature —
            // that would be a function *type*, which this oracle says nothing about.
            "variable_declarator" => self.return_type_at(node.child_by_field_name("value")?, next),
            "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "function_expression"
            // The expression form: `const g = function*() {...}`. `is_function_like` has
            // always listed it; this dispatch had not, so a call to a generator bound this
            // way fell through to `_ => None` despite the oracle treating it as function-like
            // everywhere else — addendum A1/A2 of task 4.16.
            | "generator_function"
            | "arrow_function"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature" => self.signature_return(node, next),
            _ => None,
        }
    }

    /// The return type of a function-like node: its annotation, or what its body returns.
    ///
    /// The annotation wins wherever both are present, on the same reasoning
    /// [`Self::declaration_type`] prefers one: the annotation is what the program means, and
    /// answering from the body would describe a mistake rather than a declaration.
    fn signature_return(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        if let Some(annotation) = node.child_by_field_name("return_type") {
            // `asserts_annotation` and `type_predicate_annotation` are the other two kinds
            // this field can hold (`node-types.json`); `annotation_child` hands back whatever
            // is there and the annotation vocabulary answers `None` for both, which is right —
            // `x is Foo` is not a type any rule built on this oracle asks about.
            return self.annotation_type(annotation_child(annotation)?, depth);
        }

        // An `async` function's value is a `Promise<…>` and a generator's is a `Generator<…>`,
        // and this oracle has no variant that can say either — no type arguments, no
        // `Promise`. With no annotation there is nothing here able to name the wrapper, so the
        // body's `return` type is not the call's type: answering `number` for
        // `async function rate() { return 1 }` would be a claim a rule can compare against a
        // `number` and be wrong about every time, with nothing in the answer to say a wrapper
        // was dropped. The annotation path above is untouched — the refusal is about the
        // absence of an annotation rather than about `async`.
        if wraps_its_return(node) {
            return None;
        }

        let body = node.child_by_field_name("body")?;
        if body.kind() != "statement_block" {
            // A concise arrow body is the returned expression itself.
            return self.type_of_at(body, depth);
        }

        let mut returns = Vec::new();
        collect_returns(body, &mut returns);
        if returns.is_empty() {
            // No `return` at all. `void` would be a guess, and this oracle has no variant for
            // it — see `Primitive`'s own documentation on why `any` and `unknown` are absent
            // for the same reason.
            return None;
        }

        // Every member or none, exactly as a union annotation is read: a member that could
        // not be typed leaves an answer byte-identical to a complete one, with nothing left
        // to say something was lost.
        let members: Vec<Type> = returns
            .into_iter()
            .map(|returned| match returned {
                // A bare `return;` yields `undefined`, which is a member rather than a gap.
                None => Some(Type::Primitive(Primitive::Undefined)),
                Some(expression) => self.type_of_at(expression, depth),
            })
            .collect::<Option<Vec<Type>>>()?;
        Type::union(members)
    }

    fn type_of_at(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return self.exhaust();
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
                // An imported value's declaration is in another file. With resolution
                // attached, that file is opened and the declaration typed in its own context;
                // without it, this is the `None` it always was.
                //
                // Asked here rather than in `declaration_type`'s `import_statement` arm — the
                // seam the design named — because the module specifier and *which* export was
                // imported are what `resolve` answers, and the `import_statement` node alone
                // does not say which of its specifiers bound this use.
                if let Some(Binding::Import { module, name }) =
                    self.resolver.resolve(self.tree, self.source, node)
                    && let (Some(file), Some(imports)) = (self.file, self.imports)
                {
                    return imports.imported_value_type(
                        file,
                        &module,
                        &name,
                        depth.saturating_add(1),
                    );
                }
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
    ///
    /// A declaration that binds through a *pattern* gives nothing at all. Both arms below
    /// hold a type for the thing being destructured and none for the names taken out of
    /// it, and the two are not the same type — reading either the annotation or the
    /// initializer would hand every name the whole thing's type. See [`binds_one_name`].
    fn declaration_type(&self, declaration: Node<'t>, depth: u32) -> Option<Type> {
        if depth >= MAX_DEPTH {
            return self.exhaust();
        }
        let next = depth.saturating_add(1);

        match declaration.kind() {
            "required_parameter" | "optional_parameter" => {
                if !binds_one_name(declaration, "pattern") {
                    return None;
                }
                // The `type` field is the `type_annotation` wrapper; the parameter node
                // itself is not one, so it has to be read before unwrapping. An unannotated
                // parameter has no `type` field and gives nothing, which is correct — this
                // milestone does not infer a parameter's type from its call sites.
                let annotation = declaration.child_by_field_name("type")?;
                self.annotation_type(annotation_child(annotation)?, next)
            }

            "variable_declarator" => {
                if !binds_one_name(declaration, "name") {
                    return None;
                }
                if let Some(annotation) = declaration.child_by_field_name("type") {
                    return self.annotation_type(annotation_child(annotation)?, next);
                }
                self.type_of_at(declaration.child_by_field_name("value")?, next)
            }

            // An import's declaration is in another file, which this oracle does not open.
            // A function or class declaration names a callable or a constructor rather than
            // a value with a type this milestone reasons about. A `type_parameter` is
            // whatever the call site chose, which this oracle does not see.
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
            return self.exhaust();
        }

        match node.kind() {
            // Matched on text, not kind: `any` and `unknown` parse identically to `number`.
            // Both give nothing, deliberately — `any` is the absence of a claim, and
            // `void` and `never` are types no rule built on this oracle asks about.
            //
            // There is no `bigint` row, and its absence is the measurement rather than an
            // oversight: this grammar does not lex `bigint` as a `predefined_type` at all.
            // The `type_identifier` arm below is where it is answered, and
            // `each_predefined_type_annotation_is_its_primitive` is what would redden if a
            // grammar bump moved it here.
            "predefined_type" => match self.text(node) {
                "number" => Some(Type::Primitive(Primitive::Number)),
                "string" => Some(Type::Primitive(Primitive::String)),
                "boolean" => Some(Type::Primitive(Primitive::Boolean)),
                "symbol" => Some(Type::Primitive(Primitive::Symbol)),
                _ => None,
            },

            // Every member or none.
            //
            // A member the oracle cannot type used to be dropped, on the reasoning that
            // `number | Foo<T>` still tells a rule asking "can this be a number" something
            // true. It does not: what came back was a bare `Primitive(Number)`, identical
            // in every byte to a declared `number`, with nothing left to say a member had
            // been lost. A rule reporting "this is typed `number`" then fires on
            // `amount: number | Decimal` and accuses correct code.
            //
            // A `comment` is a *named* child of a `union_type` — measured:
            // `number /* c */ | string` gives `(union_type (predefined_type) (comment)
            // (predefined_type))` — so it has to be skipped by name. Left in, it would be
            // an untypeable member, and a comment written inside an annotation would
            // silence the whole union.
            "union_type" => {
                let next = depth.saturating_add(1);
                let mut cursor = node.walk();
                let members: Vec<Type> = node
                    .children(&mut cursor)
                    .filter(|child| child.is_named() && child.kind() != "comment")
                    .map(|member| self.annotation_type(member, next))
                    .collect::<Option<Vec<Type>>>()?;
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

    /// A type named by an identifier: a same-file alias followed, or a nominal type.
    ///
    /// An alias is followed because `type Amount = number` means a rule asking "is this a
    /// number" should hear yes. An imported alias is followed too, through the
    /// [`ImportResolution`] hook, when one is installed; with none installed it stays nominal,
    /// since there is nothing here to cross the file boundary with.
    ///
    /// A *type parameter* is the one declaration that is neither. `Nominal` is a claim —
    /// that this is a distinct named type — and `f<number>(1)` makes it false, so the `T`
    /// in `function f<T>(x: T)` gives nothing at all. Which is also why the resolver has to
    /// see type parameters in the first place: before it did, the scope walk escaped
    /// outward and `type A = number; function f<A>(x: A)` typed `x` as `number`.
    fn named_type(&self, node: Node<'t>, depth: u32) -> Option<Type> {
        let name = self.text(node);
        if name.is_empty() {
            return None;
        }

        if let Some(declaration) = self.resolver.declaration_of(self.tree, self.source, node) {
            if declaration.kind() == "type_parameter" {
                return None;
            }
            if declaration.kind() == "type_alias_declaration"
                && let Some(value) = declaration.child_by_field_name("value")
            {
                return self.annotation_type(value, depth.saturating_add(1));
            }
        }

        // An imported *alias* is followed across the boundary exactly as a same-file one is
        // above: `export type Amount = number` means a rule asking "is this a number" should
        // hear yes wherever the alias was written. Everything else keeps its own nominal
        // identity and gains only a better `symbol` — see `ImportResolution`'s own doc for
        // why replacing an imported class with its declaration would be a false positive
        // rather than a better answer.
        //
        // `Exhausted` answers `None` rather than falling to the nominal case below: the name
        // *is* an alias, and a chain the bound cut is unknown, never a guess — see
        // `Followed`'s own documentation.
        if let Some(Binding::Import {
            module,
            name: imported,
        }) = self.resolver.resolve(self.tree, self.source, node)
            && let (Some(file), Some(imports)) = (self.file, self.imports)
        {
            match imports.imported_alias_type(file, &module, &imported, depth.saturating_add(1)) {
                Followed::Type(aliased) => return Some(aliased),
                Followed::Exhausted => return self.exhaust(),
                Followed::NotAnAlias => {}
            }
        }

        Some(Type::Nominal {
            name: name.to_owned(),
            symbol: self.symbol_at(node),
        })
    }

    /// Where the name at `node` came from, when the resolver can say.
    ///
    /// `exported` is the name the *declaring* module uses. With resolution attached it is
    /// followed through every re-export to the file that declares the thing, so
    /// `import Big from 'decimal.js'` reports `Big`'s real declared name rather than the
    /// placeholder `default` — which is what lets a rule compare against a required export
    /// name without accusing a conforming default import. Without resolution, or when the
    /// declaration file is unreadable, it falls back to what the import statement itself
    /// says.
    fn symbol_at(&self, node: Node<'t>) -> Option<Symbol> {
        let name = self.text(node);
        if name.is_empty() {
            return None;
        }
        let (module, exported) = match self.resolver.resolve(self.tree, self.source, node)? {
            Binding::Import {
                module,
                name: imported,
            } => {
                let declared = self
                    .file
                    .zip(self.imports)
                    .and_then(|(file, imports)| imports.imported_export(file, &module, &imported))
                    .map(|target| target.name);
                let exported = declared.or(match &imported {
                    // Copied even when no rename happened: the consumer compares
                    // `exported === require.name`, and a `None`-when-unrenamed contract makes
                    // a forgotten fallback a silent false negative on every plain import.
                    ImportedName::Named(exported) => Some(exported.clone()),
                    ImportedName::Default => Some("default".to_owned()),
                    // `import * as D` binds the module object; there is no one exported name.
                    ImportedName::Namespace => None,
                });
                (Some(module), exported)
            }
            Binding::Local(_) => (None, None),
        };
        Some(Symbol {
            name: name.to_owned(),
            module,
            exported,
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

/// Whether a declaration binds exactly one name, rather than destructuring.
///
/// The resolver answers `declaration_of` with the whole declaration for every name a
/// pattern binds, so `const { rate }: Money = order` hands back the same declarator for
/// `rate` that `const order: Money = row` hands back for `order`. Nothing further down
/// distinguishes them, and both the annotation and the initializer describe the thing
/// being taken apart rather than any name taken out of it: without this guard,
/// `const s = String(q); const { length } = s` types `length` as `string`, and
/// `function f({ rate }: Money)` types `rate` as `Money`. Both are confidently wrong,
/// which is worse than the `None` this produces instead.
///
/// Measured against tree-sitter-typescript: the named field is an `identifier` for a plain
/// binding — including `let a!: number`, whose definite-assignment `!` does not change the
/// kind — and an `object_pattern`, `array_pattern` or `rest_pattern` for the rest. So the
/// test is for the one shape that is not a pattern, not against a list of the ones that
/// are; a pattern kind this file has never heard of still fails it.
///
/// Typing a destructured name needs property lookup on the pattern's type, which is a
/// later milestone's capability rather than a gap here.
fn binds_one_name(declaration: Node<'_>, field: &str) -> bool {
    declaration
        .child_by_field_name(field)
        .is_some_and(|bound| bound.kind() == "identifier")
}

/// Whether a function-like node's call yields a wrapper around what its body returns.
///
/// `async` and `*` are anonymous tokens rather than fields — the grammar writes them bare, the
/// same way `export default`'s `default` is written — so this reads the children rather than
/// asking for a field that does not exist. Both spellings of a generator are covered: the
/// dedicated `generator_function*` kinds and a `method_definition` or arrow carrying the token.
fn wraps_its_return(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| !child.is_named() && matches!(child.kind(), "async" | "*"))
}

/// Every `return` in this body, skipping the ones that belong to a nested function.
///
/// `None` for a bare `return;`. Nested functions are skipped because their returns are
/// somebody else's: `function f() { const g = () => 'a'; return 1; }` returns a number, and a
/// walk that took every `return_statement` under the body would answer `number | string`.
///
/// A stack rather than a cursor recursion, and children pushed in reverse so the walk visits
/// them in source order — the union is canonicalized afterwards, so this is about a
/// reproducible *failure* message rather than about the answer.
fn collect_returns<'t>(node: Node<'t>, out: &mut Vec<Option<Node<'t>>>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "return_statement" {
            out.push(current.named_child(0));
            continue;
        }
        if current.id() != node.id() && is_function_like(current) {
            continue;
        }
        let mut cursor = current.walk();
        let children: Vec<Node<'t>> = current.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

/// Whether a node introduces a function of its own.
fn is_function_like(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature"
    )
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
