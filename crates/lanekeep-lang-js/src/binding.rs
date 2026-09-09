//! Binding resolution for JavaScript and TypeScript.
//!
//! Resolution walks outward from an identifier, asking each enclosing scope whether it
//! declares that name. The innermost scope that does is the answer, which is what gives
//! shadowing the right result without building a full symbol table.
//!
//! It is deliberately syntactic. Nothing here follows a re-export, evaluates a dynamic
//! import, or knows a type. Architecture §1 draws that line, and the two cases that
//! actually matter are within it: an aliased import must resolve to what it aliases, and a
//! local declaration must not be mistaken for an import of the same name.

use lanekeep_lang::binding::{Binding, BindingKind, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

/// Syntactic binding resolution for the JavaScript family.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsBindingResolver;

/// Node kinds that introduce a scope.
///
/// `statement_block` covers function bodies and bare blocks alike, which is why function
/// nodes appear here only for their parameters.
const SCOPE_KINDS: &[&str] = &[
    "program",
    "statement_block",
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
    "class_declaration",
    "class",
    // Kinds that carry a `type_parameters` field and were not scopes.
    // `tree-sitter-typescript` 0.23.2's `typescript/src/node-types.json` declares the field
    // on eighteen node kinds; eight were already above, four landed in #207 and the last six
    // in #208, so every carrier is a scope and nothing is outstanding.
    //
    // What the omission produced was worse than a missing answer, which is why the list is
    // derived from the grammar's own declaration rather than from anyone's reading:
    // `type A = number; interface I { m<A>(x: A): A }` typed the annotation `number`, a
    // confident answer identical in every byte to a declared one.
    //
    // Do not derive this list from a hand-written parse sample: the first attempt at this
    // fix did exactly that and reported twelve carriers, because the sample omitted every
    // signature-shaped and type-shaped generic construct — a probe whose sample does not
    // parse, or simply does not cover a construct, silently subtracts a kind, and there is
    // no way to tell the difference between "not a carrier" and "not exercised" from the
    // count alone. Reading `node-types.json`, where the field is declared, is the correct
    // method; a sample can always be incomplete.
    //
    // `function_signature` is the one of the four that also carries `parameters`, so adding
    // it makes an ambient function's parameters resolvable for the first time as well. That
    // is a widening beyond the fix and it is wanted: `declare function credit(amount:
    // number)` declares money as a number exactly as the non-ambient form does.
    "abstract_class_declaration",
    "interface_declaration",
    "type_alias_declaration",
    "function_signature",
    // #208's first two. `method_signature` and `abstract_method_signature` are ordinary
    // members of a body the walk already descends, and they are the two that produce the
    // reproducer: `type A = number; interface I { m<A>(x: A): A }` resolved `A` to the
    // alias, because the type parameters are on the signature and `interface_declaration`
    // — a scope since #207 — does not carry them.
    //
    // Both also carry `parameters`, so this widens as well as fixes: a parameter annotated
    // inside an interface method or an abstract member is typed where it used to give
    // nothing, and `lanekeep/no-restricted-types` reports it. Measured per kind, in the
    // pull request body.
    "method_signature",
    "abstract_method_signature",
    // #208's remaining four, landing together. `call_signature` and `construct_signature`
    // are members of an interface body; `constructor_type` and `function_type` sit in pure
    // type position, where nothing had ever asked this walk for a scope before.
    "call_signature",
    "construct_signature",
    "constructor_type",
    "function_type",
    "catch_clause",
    "for_statement",
    "for_in_statement",
];

impl JsBindingResolver {
    /// Every enclosing scope, innermost first.
    fn scopes(node: Node<'_>) -> Vec<Node<'_>> {
        let mut out = Vec::new();
        let mut current = node.parent();
        while let Some(scope) = current {
            if SCOPE_KINDS.contains(&scope.kind()) {
                out.push(scope);
            }
            current = scope.parent();
        }
        out
    }

    /// What `name` is bound to in this scope, ignoring nested scopes.
    fn declaration_in(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
        Self::declaration_entry(scope, source, name).map(|(_, binding)| binding)
    }

    /// The node that declares `name` in this scope, ignoring nested scopes.
    fn declaration_node_in<'t>(scope: Node<'t>, source: &str, name: &str) -> Option<Node<'t>> {
        Self::declaration_entry(scope, source, name).map(|(node, _)| node)
    }

    /// The declaration of `name` in this scope: the node, and what it binds.
    ///
    /// One walk with two projections above it, rather than two walks. Two would be free to
    /// disagree about whether a name is declared at all, and the disagreement would be
    /// silent — the binding half decides whether a rule matches, the node half decides
    /// whether a type can be read, and a file where they differ answers one question and
    /// not the other with nothing to say so.
    fn declaration_entry<'t>(
        scope: Node<'t>,
        source: &str,
        name: &str,
    ) -> Option<(Node<'t>, Binding)> {
        let mut cursor = scope.walk();

        // Parameters belong to the function, not to its body block. The *parameter* is
        // returned rather than the list holding it, because an annotation belongs to one
        // parameter and a caller handed the list would have to search it again.
        if let Some(params) = scope.child_by_field_name("parameters") {
            let mut params_cursor = params.walk();
            if let Some(parameter) = params
                .children(&mut params_cursor)
                .find(|child| child.is_named() && pattern_binds(*child, source, name))
            {
                return Some((parameter, Binding::Local(BindingKind::Param)));
            }
        }
        // A generic type parameter belongs to the signature that declares it, exactly as a
        // value parameter does — `type_parameters` is a sibling field of `parameters` on
        // every scope kind that can carry one. Without this the name is invisible here and
        // the walk escapes outward, so `type A = number; function f<A>(x: A)` answers with
        // the outer alias: a confidently wrong type, where the honest answer is that a type
        // parameter is whatever the call site chose.
        //
        // `BindingKind::TypeParam` already exists for this, added for Go's `func F[T any]`
        // and carried end to end through the WIT `binding-kind` enum. There is no new
        // variant and no host-API change here.
        //
        // After the value parameters rather than before, so that a signature perverse
        // enough to write both — `function f<T>(T: number)` — keeps the answer it gave
        // before this arm existed. The resolver is namespace-blind, so one of the two has
        // to win, and the one that already did is the safer choice.
        if let Some(type_params) = scope.child_by_field_name("type_parameters") {
            let mut type_params_cursor = type_params.walk();
            if let Some(parameter) = type_params
                .children(&mut type_params_cursor)
                .filter(|child| child.kind() == "type_parameter")
                .find(|child| {
                    child
                        .child_by_field_name("name")
                        .is_some_and(|bound| node_text(bound, source) == name)
                })
            {
                return Some((parameter, Binding::Local(BindingKind::TypeParam)));
            }
        }
        // Let-chains from here down, where this used to read `is_some_and`: the old comment
        // explained they were unavailable at the declared MSRV and that working code is not
        // rewritten just because new syntax becomes legal — both still true. What changed is
        // the shape, not the syntax budget: this walk now hands back the bound *node*, and
        // `is_some_and` collapses the `Some` it matched down to a `bool`, which has nowhere
        // to keep a node for the caller. A let-chain keeps `parameter` alive to return.
        //
        // An arrow function with a single unparenthesized parameter.
        if scope.kind() == "arrow_function"
            && let Some(parameter) = scope.child_by_field_name("parameter")
            && node_text(parameter, source) == name
        {
            return Some((parameter, Binding::Local(BindingKind::Param)));
        }
        if scope.kind() == "catch_clause"
            && let Some(parameter) = scope.child_by_field_name("parameter")
            && pattern_binds(parameter, source, name)
        {
            return Some((parameter, Binding::Local(BindingKind::CatchParam)));
        }
        // A `for...of` / `for...in` head, which `for_statement`'s does not cover. A `for`
        // loop's initializer really is a `lexical_declaration` child, so the walk below
        // finds it; measured, both `for (const x of ns)` and `for (x in ns)` are
        // `(for_in_statement left: (identifier) right: (identifier) body: ...)` with no
        // declaration node anywhere for that walk to reach. So the loop variable was
        // invisible and the scope walk escaped outward: `const x = 'a'; for (const x of ns)
        // { g(x) }` resolved the inner `x` to the outer `const`.
        //
        // The `kind` field is what separates a head that declares from one that does not.
        // It is the `const` / `let` / `var` token itself, present for the first and absent
        // for the second — and `for (x of ns)` genuinely binds nothing, it assigns to a
        // name that already exists, so binding it here would invent a shadow that is not in
        // the program.
        //
        // `left` is returned rather than the statement holding it, for the reason the
        // parameter walk above gives: it is the node that binds, and a caller handed the
        // statement would have to find it again. There is nothing further to read either
        // way — measured, `for (const x: number of ns)` does not parse at all, coming back
        // as an `ERROR` node rather than an annotated head, and the element type of `right`
        // is a capability no milestone has yet.
        if scope.kind() == "for_in_statement"
            && scope.child_by_field_name("kind").is_some()
            && let Some(left) = scope.child_by_field_name("left")
            && pattern_binds(left, source, name)
        {
            return Some((left, Binding::Local(declaration_kind(scope, source))));
        }

        for child in scope.children(&mut cursor) {
            if let Some(entry) = declaration_entry_of(child, source, name) {
                return Some(entry);
            }
        }
        None
    }
}

/// The declaration of `name` by this statement: the node, and what it binds.
fn declaration_entry_of<'t>(
    node: Node<'t>,
    source: &str,
    name: &str,
) -> Option<(Node<'t>, Binding)> {
    match node.kind() {
        "import_statement" => import_binding(node, source, name).map(|binding| (node, binding)),

        "lexical_declaration" | "variable_declaration" => {
            let kind = declaration_kind(node, source);
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
                .find(|declarator| {
                    declarator
                        .child_by_field_name("name")
                        .is_some_and(|pattern| pattern_binds(pattern, source, name))
                })
                .map(|declarator| (declarator, Binding::Local(kind)))
        }

        // A function binds its name in every form the grammar spells it: the plain and
        // generator declarations, and the ambient `declare function f(a: number): void`,
        // which is a `function_signature` rather than a `function_declaration` —
        // `node-types.json` gives the kind a required `name` (`identifier`) beside the
        // `parameters` field that made it a scope back in #207.
        "function_declaration" | "generator_function_declaration" | "function_signature" => {
            named_as(node, source, name).then_some((node, Binding::Local(BindingKind::Function)))
        }

        // A class and its `abstract` spelling are two kinds — there is no
        // `class_declaration` node for an abstract class, so without the second here the
        // walk matched on kind and a use of the name escaped outward to nothing — and both
        // bind the name the same way.
        "class_declaration" | "abstract_class_declaration" => {
            named_as(node, source, name).then_some((node, Binding::Local(BindingKind::Class)))
        }

        // A type alias binds a name in the type namespace. `BindingKind::Type` already
        // exists for exactly this construct — its own doc comment names `type T = U`
        // aliases directly, Rust's resolver already maps `type Alias = u8;` to it, and the
        // WIT `binding-kind` enum and host conversion already carry it end to end, so
        // there is no new variant and no host-API change to make here. Reusing `Class`
        // would misreport a type alias as a class to any rule asking `bindingKind` — the
        // exact failure the `Type`/`Receiver`/`TypeParam` kinds above were added to avoid:
        // "the nearest existing kind would be a lie".
        //
        // An interface and an enum name a type by the same convention, in one arm here as
        // Rust's resolver puts `struct_item`/`enum_item` in one — "a struct, enum, union or
        // alias all name a type". The grammar hands each kind a required `name` field (the
        // interface's a `type_identifier`, the enum's an `identifier`) exactly as the alias
        // does.
        "type_alias_declaration" | "interface_declaration" | "enum_declaration" => {
            named_as(node, source, name).then_some((node, Binding::Local(BindingKind::Type)))
        }

        // `namespace P` and `module P` both bind a module name. Either kind's `name` may be
        // an `identifier`, a `nested_identifier` or a `string`, and a string's node text
        // carries the quotes themselves — `declare module "p"` declares `p`, not `"p"` — so
        // the comparison goes through `trim_quotes`, which the import arm above already
        // reads module specifiers through for the same reason.
        "module" | "internal_module" => named_unquoted_as(node, source, name)
            .then_some((node, Binding::Local(BindingKind::Module))),

        // `declare` wraps whatever it applies to and has no name of its own, so the answer
        // is whatever the wrapped declaration answers: `declare const r: number` binds `r`
        // as a const, `declare function f(): void` as a function. The wrapped declaration
        // is the first *named* child — the `declare` token itself is anonymous, and
        // `node-types.json` gives the kind no fields at all.
        "ambient_declaration" => {
            wrapped_declaration(node).and_then(|inner| declaration_entry_of(inner, source, name))
        }

        // A top-level `namespace` is the one declaration that is not a statement in this
        // grammar: measured against 0.23.2, `namespace P {}` parses as an
        // `expression_statement` wrapping the `internal_module`, where `module P {}` — and
        // every other declaration kind — is a direct child of `program`. Without looking
        // through the statement, the `internal_module` arm above is unreachable for exactly
        // the construct it exists for, and a use of a namespace's name walks out of the
        // file to nothing. Nothing else declares through an expression, so the recursion
        // answers for namespaces alone.
        "expression_statement" => node
            .named_child(0)
            .and_then(|inner| declaration_entry_of(inner, source, name)),

        // `export const x = 1`, `export function f() {}` — the declaration is inside.
        "export_statement" => node
            .child_by_field_name("declaration")
            .and_then(|inner| declaration_entry_of(inner, source, name)),

        _ => None,
    }
}

fn named_as(node: Node<'_>, source: &str, name: &str) -> bool {
    node.child_by_field_name("name")
        .is_some_and(|n| node_text(n, source) == name)
}

/// Whether a declaration's `name` field is `name`, once its quotes are gone.
///
/// `module`'s and `internal_module`'s `name` may be a `string` as well as an identifier,
/// and a `string` node's text includes the quotes, so the plain [`named_as`] comparison
/// would answer `"p" != p`. `trim_quotes` is what the import arm reads module specifiers
/// through, for the same reason.
fn named_unquoted_as(node: Node<'_>, source: &str, name: &str) -> bool {
    node.child_by_field_name("name")
        .is_some_and(|n| trim_quotes(node_text(declared_segment(n), source)) == name)
}

/// The node whose text is the name a `module` or `internal_module` declares.
///
/// `namespace A.B {}` is shorthand for `namespace A { namespace B {} }`: the enclosing scope
/// sees `A`, and the dotted spelling binds nothing. The grammar hands the whole `A.B` over as
/// a `nested_identifier` whose `object` is the left part — and every level *inside* that is
/// aliased to `member_expression` (`grammar.json`'s `nested_identifier` rule), so
/// `google.maps.places` is a `nested_identifier` over the member expression `google.maps`.
/// Both kinds carry an `object` field, and the declared name is the leftmost segment.
fn declared_segment(name: Node<'_>) -> Node<'_> {
    let mut node = name;
    while matches!(node.kind(), "nested_identifier" | "member_expression") {
        match node.child_by_field_name("object") {
            Some(object) => node = object,
            None => break,
        }
    }
    node
}

/// The declaration a `declare` applies to: the first named child that is not a comment.
///
/// `ambient_declaration` has no fields at all, and a comment is a named extra in this
/// grammar, so `named_child(0)` of `declare /* c */ const r` is the comment. A JSDoc block
/// between `declare` and the declaration is an ordinary shape in a `.d.ts`, and skipping it
/// is what keeps such a file's declarations bound.
fn wrapped_declaration(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() != "comment")
}

/// `const` or `let` for a lexical declaration, `var` otherwise.
///
/// Also reads a `for...of` / `for...in` head, whose `const` / `let` / `var` sits among its
/// unnamed children the same way a declaration's does. The `var` fallback is unreachable
/// from there — that caller has already established the `kind` field is present, and the
/// field *is* the keyword.
fn declaration_kind(node: Node<'_>, source: &str) -> BindingKind {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            continue;
        }
        match node_text(child, source) {
            "const" => return BindingKind::Const,
            "let" => return BindingKind::Let,
            "var" => return BindingKind::Var,
            _ => {}
        }
    }
    BindingKind::Var
}

/// Whether a binding pattern introduces `name`.
///
/// Handles destructuring, so `const { a, b: c } = x` binds `a` and `c` — a rule that only
/// understood plain identifiers would silently miss both.
fn pattern_binds(pattern: Node<'_>, source: &str, name: &str) -> bool {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            node_text(pattern, source) == name
        }
        // `{ b: c }` binds `c`; `[x]` binds `x`; `a = 1` binds `a`; `...rest` binds `rest`.
        "pair_pattern" | "object_assignment_pattern" => pattern
            .child_by_field_name("value")
            .is_some_and(|value| pattern_binds(value, source, name)),
        _ => {
            let mut cursor = pattern.walk();
            pattern
                .children(&mut cursor)
                .any(|child| child.is_named() && pattern_binds(child, source, name))
        }
    }
}

/// What an import statement binds `name` to.
fn import_binding(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    let module = node
        .child_by_field_name("source")
        .map(|source_node| trim_quotes(node_text(source_node, source)))?
        .to_owned();

    let mut cursor = node.walk();
    let clause = node
        .children(&mut cursor)
        .find(|c| c.kind() == "import_clause")?;

    let mut clause_cursor = clause.walk();
    for child in clause.children(&mut clause_cursor) {
        match child.kind() {
            // `import d from 'm'`
            "identifier" if node_text(child, source) == name => {
                return Some(Binding::Import {
                    module,
                    name: ImportedName::Default,
                });
            }
            // `import * as ns from 'm'`
            "namespace_import" => {
                let mut inner = child.walk();
                if child
                    .children(&mut inner)
                    .any(|n| n.kind() == "identifier" && node_text(n, source) == name)
                {
                    return Some(Binding::Import {
                        module,
                        name: ImportedName::Namespace,
                    });
                }
            }
            // `import { a, b as c } from 'm'`
            "named_imports" => {
                let mut inner = child.walk();
                for specifier in child
                    .children(&mut inner)
                    .filter(|n| n.kind() == "import_specifier")
                {
                    let imported = specifier
                        .child_by_field_name("name")
                        .map(|n| trim_quotes(node_text(n, source)).to_owned())?;
                    // The alias is what the local name is; without one they are the same.
                    let local = specifier
                        .child_by_field_name("alias")
                        .map_or(imported.clone(), |n| node_text(n, source).to_owned());

                    if local == name {
                        return Some(Binding::Import {
                            module,
                            name: ImportedName::Named(imported),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn node_text<'src>(node: Node<'_>, source: &'src str) -> &'src str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn trim_quotes(text: &str) -> &str {
    text.trim_matches(|c| c == '\'' || c == '"' || c == '`')
}

impl BindingResolver for JsBindingResolver {
    fn resolve(&self, _tree: &Tree, source: &str, node: Node<'_>) -> Option<Binding> {
        let name = node_text(node, source);
        if name.is_empty() {
            return None;
        }

        Self::scopes(node)
            .into_iter()
            .find_map(|scope| Self::declaration_in(scope, source, name))
    }

    fn is_shadowed(&self, _tree: &Tree, source: &str, node: Node<'_>) -> bool {
        let name = node_text(node, source);
        if name.is_empty() {
            return false;
        }

        // More than one enclosing scope declaring the name means the innermost one is
        // hiding an outer one. A name declared once is not shadowing anything, even
        // though it is perfectly local.
        Self::scopes(node)
            .into_iter()
            .filter(|scope| Self::declaration_in(*scope, source, name).is_some())
            .count()
            > 1
    }

    /// The resolver's own entry walk, projected for a caller that holds a candidate
    /// declaration: `lanekeep-types`' export lookup asks this of every top-level statement,
    /// and a second kind table there drifted from this one once already (#229). The
    /// `Binding::Local` filter is what keeps an `import_statement` from answering as a
    /// declaration.
    fn declares<'t>(&self, source: &str, node: Node<'t>, name: &str) -> Option<Node<'t>> {
        declaration_entry_of(node, source, name)
            .filter(|(_, binding)| matches!(binding, Binding::Local(_)))
            .map(|(node, _)| node)
    }

    fn declaration_of<'t>(
        &self,
        _tree: &'t Tree,
        source: &str,
        node: Node<'t>,
    ) -> Option<Node<'t>> {
        let name = node_text(node, source);
        if name.is_empty() {
            return None;
        }

        Self::scopes(node)
            .into_iter()
            .find_map(|scope| Self::declaration_node_in(scope, source, name))
    }
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;

    use super::*;
    use crate::TypeScript;

    /// Resolve the last identifier in the source that reads exactly `name`.
    ///
    /// Rules resolve a *use*, so tests must too — resolving the declaration site would
    /// exercise a different path than the one that matters.
    fn resolve_use(source: &str, name: &str) -> Option<Binding> {
        with_use(source, name, |tree, node| {
            JsBindingResolver.resolve(tree, source, node)
        })
    }

    fn shadowed(source: &str, name: &str) -> bool {
        with_use(source, name, |tree, node| {
            JsBindingResolver.is_shadowed(tree, source, node)
        })
    }

    /// The node-returning twin of `resolve_use`.
    fn declaration_use(source: &str, name: &str) -> Option<String> {
        with_use(source, name, |tree, node| {
            JsBindingResolver
                .declaration_of(tree, source, node)
                .map(|declaration| declaration.kind().to_owned())
        })
    }

    fn with_use<T>(source: &str, name: &str, f: impl Fn(&Tree, Node<'_>) -> T) -> T {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");

        // The last occurrence *by position*, not by traversal order — the traversal is
        // depth-first over a stack, so "last visited" is not "last in the file", and the
        // tests mean the latter.
        //
        // `type_identifier` alongside `identifier`: a name that lives only in the type
        // namespace, like a type alias, never appears as a plain `identifier` node — the
        // grammar tokenizes both its declaration and its uses as `type_identifier` instead.
        // `resolve`/`declaration_of` read a node's text and walk its parents without caring
        // which of the two kinds it is, so the test helper matches that indifference.
        let mut found: Option<Node<'_>> = None;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "identifier" | "type_identifier")
                && node_text(node, source) == name
                && found.is_none_or(|best| node.start_byte() > best.start_byte())
            {
                found = Some(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        let node = found.unwrap_or_else(|| panic!("no identifier `{name}` in source"));
        f(&tree, node)
    }

    fn import(module: &str, name: ImportedName) -> Binding {
        Binding::Import {
            module: module.to_owned(),
            name,
        }
    }

    // --- imports ---------------------------------------------------------------------

    #[test]
    fn resolves_a_named_import() {
        assert_eq!(
            resolve_use(
                "import { makeStyles } from '@rneui/themed';\nmakeStyles();",
                "makeStyles"
            ),
            Some(import(
                "@rneui/themed",
                ImportedName::Named("makeStyles".to_owned())
            ))
        );
    }

    #[test]
    fn resolves_an_aliased_import_to_what_it_aliases() {
        // The case that makes this whole module necessary. A rule looking for
        // `makeStyles` must find `ms`, and must learn that `ms` *is* `makeStyles`.
        assert_eq!(
            resolve_use(
                "import { makeStyles as ms } from '@rneui/themed';\nms();",
                "ms"
            ),
            Some(import(
                "@rneui/themed",
                ImportedName::Named("makeStyles".to_owned())
            ))
        );
    }

    #[test]
    fn resolves_a_default_import() {
        assert_eq!(
            resolve_use(
                "import React from 'react';\nReact.createElement();",
                "React"
            ),
            Some(import("react", ImportedName::Default))
        );
    }

    #[test]
    fn resolves_a_namespace_import() {
        assert_eq!(
            resolve_use("import * as path from 'node:path';\npath.join();", "path"),
            Some(import("node:path", ImportedName::Namespace))
        );
    }

    #[test]
    fn resolves_one_specifier_among_several() {
        let source = "import { a, b as c, d } from 'm';\nc();";
        assert_eq!(
            resolve_use(source, "c"),
            Some(import("m", ImportedName::Named("b".to_owned())))
        );
    }

    #[test]
    fn handles_both_quote_styles() {
        assert_eq!(
            resolve_use("import { a } from \"m\";\na();", "a"),
            Some(import("m", ImportedName::Named("a".to_owned())))
        );
    }

    // --- local declarations -----------------------------------------------------------

    #[test]
    fn resolves_local_declaration_kinds() {
        assert_eq!(
            resolve_use("const x = 1;\nx;", "x"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("let x = 1;\nx;", "x"),
            Some(Binding::Local(BindingKind::Let))
        );
        assert_eq!(
            resolve_use("var x = 1;\nx;", "x"),
            Some(Binding::Local(BindingKind::Var))
        );
        assert_eq!(
            resolve_use("function f() {}\nf();", "f"),
            Some(Binding::Local(BindingKind::Function))
        );
        assert_eq!(
            resolve_use("class C {}\nnew C();", "C"),
            Some(Binding::Local(BindingKind::Class))
        );
    }

    /// A type alias is its own kind, not the nearest lookalike.
    ///
    /// `type Alias = number;` binds like a declaration, but it is not a class — a rule
    /// doing `ctx.bindingKind(n) === "class"` to find real classes must not also match
    /// every type alias in the file.
    #[test]
    fn a_type_alias_resolves_as_the_type_kind_not_class() {
        assert_eq!(
            resolve_use("type Alias = number;\nlet x: Alias;", "Alias"),
            Some(Binding::Local(BindingKind::Type))
        );
    }

    #[test]
    fn resolves_parameters() {
        assert_eq!(
            resolve_use("function f(a) { return a; }", "a"),
            Some(Binding::Local(BindingKind::Param))
        );
        assert_eq!(
            resolve_use("const g = (b) => b;", "b"),
            Some(Binding::Local(BindingKind::Param))
        );
        assert_eq!(
            resolve_use("const h = c => c;", "c"),
            Some(Binding::Local(BindingKind::Param))
        );
    }

    #[test]
    fn resolves_a_catch_parameter() {
        assert_eq!(
            resolve_use("try { f(); } catch (err) { report(err); }", "err"),
            Some(Binding::Local(BindingKind::CatchParam))
        );
    }

    #[test]
    fn resolves_through_destructuring() {
        // A resolver that only understood plain identifiers would silently miss every one
        // of these, and a rule built on it would quietly under-report.
        assert_eq!(
            resolve_use("const { a } = obj;\na;", "a"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("const { b: renamed } = obj;\nrenamed;", "renamed"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("const [first] = arr;\nfirst;", "first"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("const { ...rest } = obj;\nrest;", "rest"),
            Some(Binding::Local(BindingKind::Const))
        );
    }

    #[test]
    fn resolves_an_exported_declaration() {
        assert_eq!(
            resolve_use("export const x = 1;\nx;", "x"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("export function f() {}\nf();", "f"),
            Some(Binding::Local(BindingKind::Function))
        );
    }

    // --- the case that makes syntactic matching wrong ------------------------------------

    #[test]
    fn a_local_declaration_wins_over_an_import_of_the_same_name() {
        // Without this, a rule keyed on the name `makeStyles` fires on a local function
        // that has nothing to do with the import — a false positive a user reads as the
        // tool being broken.
        let source = "import { makeStyles } from '@rneui/themed';\n\
                      function inner() {\n\
                        const makeStyles = () => {};\n\
                        return makeStyles();\n\
                      }";
        assert_eq!(
            resolve_use(source, "makeStyles"),
            Some(Binding::Local(BindingKind::Const))
        );
    }

    #[test]
    fn the_innermost_scope_wins() {
        let source = "const x = 1;\nfunction f() { const x = 2; return x; }";
        assert_eq!(
            resolve_use(source, "x"),
            Some(Binding::Local(BindingKind::Const))
        );

        let source = "const y = 1;\nfunction g(y) { return y; }";
        assert_eq!(
            resolve_use(source, "y"),
            Some(Binding::Local(BindingKind::Param))
        );
    }

    #[test]
    fn an_undeclared_name_resolves_to_nothing() {
        assert_eq!(resolve_use("globalThing();", "globalThing"), None);
    }

    // --- shadowing --------------------------------------------------------------------

    #[test]
    fn a_name_declared_once_is_not_shadowed() {
        assert!(!shadowed("const x = 1;\nx;", "x"));
        assert!(!shadowed("import { a } from 'm';\na();", "a"));
        assert!(!shadowed("function f(p) { return p; }", "p"));
    }

    #[test]
    fn a_name_redeclared_inside_is_shadowed() {
        assert!(shadowed(
            "const x = 1;\nfunction f() { const x = 2; return x; }",
            "x"
        ));
        assert!(shadowed(
            "import { makeStyles } from 'm';\nfunction f() { const makeStyles = 1; return makeStyles; }",
            "makeStyles"
        ));
        assert!(shadowed("const p = 1;\nfunction f(p) { return p; }", "p"));
    }

    /// A `for...of` head shadows, and a bare one does not.
    ///
    /// The loop variable was invisible before this: `for_in_statement` was in the scope
    /// list and declared nothing, so the walk went past the head to whatever was outside
    /// it. `is_shadowed` is the crisp half, because both forms below otherwise answer
    /// `Local(Const)` and only one of them declares anything.
    #[test]
    fn a_declaring_loop_head_shadows_and_a_bare_one_does_not() {
        assert!(shadowed("const x = 1;\nfor (const x of ns) { g(x); }", "x"));
        assert!(shadowed("const x = 1;\nfor (let x of ns) { g(x); }", "x"));
        assert!(shadowed("const x = 1;\nfor (var x in ns) { g(x); }", "x"));
        // No `const` / `let` / `var`: this assigns to the outer `x` rather than declaring
        // a new one, so treating it as a declaration would invent a shadow.
        assert!(!shadowed("const x = 1;\nfor (x of ns) { g(x); }", "x"));
    }

    #[test]
    fn a_loop_head_binds_with_the_keyword_it_was_written_with() {
        assert_eq!(
            resolve_use("for (const x of ns) { g(x); }", "x"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("for (let x of ns) { g(x); }", "x"),
            Some(Binding::Local(BindingKind::Let))
        );
        assert_eq!(
            resolve_use("for (var x in ns) { g(x); }", "x"),
            Some(Binding::Local(BindingKind::Var))
        );
    }

    /// A destructuring loop head binds every name the pattern does.
    #[test]
    fn a_destructuring_loop_head_binds_through_its_pattern() {
        assert_eq!(
            resolve_use("for (const { a } of xs) { g(a); }", "a"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("for (const [a, b] of xs) { g(b); }", "b"),
            Some(Binding::Local(BindingKind::Const))
        );
    }

    /// A generic type parameter belongs to the signature that declares it.
    ///
    /// Invisible before this, so the walk escaped outward and an outer alias of the same
    /// name answered instead — which a type oracle reading the answer turns into a
    /// confident, wrong type.
    #[test]
    fn a_type_parameter_binds_in_the_signature_that_declares_it() {
        for source in [
            "type A = number;\nfunction f<A>(x: A) { return x; }",
            "type A = number;\nclass C<A> { m(x: A) { return x; } }",
            "type A = number;\nclass C { m<A>(x: A) { return x; } }",
            "type A = number;\nconst f = <A,>(x: A) => x;",
        ] {
            assert_eq!(
                resolve_use(source, "A"),
                Some(Binding::Local(BindingKind::TypeParam)),
                "{source}"
            );
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_parameter".to_owned()),
                "{source}"
            );
        }
    }

    /// And it shadows the alias rather than merely coexisting with it.
    #[test]
    fn a_type_parameter_shadows_an_outer_alias_of_the_same_name() {
        assert!(shadowed(
            "type A = number;\nfunction f<A>(x: A) { return x; }",
            "A"
        ));
        assert!(!shadowed(
            "type A = number;\nfunction f(x: A) { return x; }",
            "A"
        ));
    }

    // --- the four declaration kinds that carry `type_parameters` and were not scopes -----
    //
    // `tree-sitter-typescript` 0.23.2's `typescript/src/node-types.json` declares
    // `type_parameters` on eighteen node kinds; eight were already in `SCOPE_KINDS`, and
    // these four were not, so a type parameter declared on any of them was invisible and
    // the scope walk escaped outward — the exact failure the `type_parameters` arm was
    // written to fix for functions. Every carrier the grammar declares is a scope now; the
    // two sections below cover the six that #208 added, split as they shipped.
    //
    // The sources are the same four in all three tests, deliberately. Splitting them across
    // tests would let one kind be covered in one direction and not the other, which is how
    // `class` came to be right and `abstract class` wrong in the first place.

    /// Each of the four binds its type parameter rather than letting the walk escape.
    #[test]
    fn a_type_parameter_binds_on_every_declaration_kind_that_can_declare_one() {
        for source in [
            "type A = number;\ninterface O<A> { x: A }",
            "type A = number;\ntype O<A> = { x: A };",
            "type A = number;\nabstract class C<A> { m(x: A) { return x; } }",
            "type A = number;\ndeclare function f<A>(x: A): void;",
        ] {
            assert_eq!(
                resolve_use(source, "A"),
                Some(Binding::Local(BindingKind::TypeParam)),
                "{source}"
            );
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_parameter".to_owned()),
                "{source}"
            );
        }
    }

    /// And shadows the outer alias rather than merely coexisting with it.
    #[test]
    fn a_type_parameter_on_those_kinds_shadows_an_outer_alias() {
        for source in [
            "type A = number;\ninterface O<A> { x: A }",
            "type A = number;\ntype O<A> = { x: A };",
            "type A = number;\nabstract class C<A> { m(x: A) { return x; } }",
            "type A = number;\ndeclare function f<A>(x: A): void;",
        ] {
            assert!(shadowed(source, "A"), "{source}");
        }
    }

    /// The half that keeps the fix from over-reaching.
    ///
    /// With no type parameter to shadow it, an outer alias is still what the annotation
    /// resolves to. Without this, a fix that made these kinds swallow every name would pass
    /// both tests above.
    #[test]
    fn without_a_type_parameter_the_outer_alias_still_answers() {
        for source in [
            "type A = number;\ninterface O { x: A }",
            "type A = number;\ntype O = { x: A };",
            "type A = number;\nabstract class C { m(x: A) { return x; } }",
            "type A = number;\ndeclare function f(x: A): void;",
        ] {
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_alias_declaration".to_owned()),
                "{source}"
            );
        }
    }

    // --- the six carriers #208 left, in two commits ------------------------------------
    //
    // Same three-test shape as the four above and for the same reason: splitting the
    // sources across tests would let one kind be covered in one direction and not the
    // other. `: A` is the return type in every source deliberately — a signature written
    // `: void` puts a dead `type_annotation` last in the file, which is the row #207
    // shipped twice and #208's acceptance names.

    /// Each of these binds its type parameter rather than letting the walk escape.
    #[test]
    fn a_type_parameter_binds_on_a_method_signature_kind() {
        for source in [
            "type A = number;\ninterface I { m<A>(x: A): A }",
            "type A = number;\nabstract class C { abstract m<A>(x: A): A }",
        ] {
            assert_eq!(
                resolve_use(source, "A"),
                Some(Binding::Local(BindingKind::TypeParam)),
                "{source}"
            );
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_parameter".to_owned()),
                "{source}"
            );
        }
    }

    /// And shadows the outer alias rather than merely coexisting with it.
    #[test]
    fn a_type_parameter_on_a_method_signature_kind_shadows_an_outer_alias() {
        for source in [
            "type A = number;\ninterface I { m<A>(x: A): A }",
            "type A = number;\nabstract class C { abstract m<A>(x: A): A }",
        ] {
            assert!(shadowed(source, "A"), "{source}");
        }
    }

    /// The half that keeps the fix from over-reaching: with no type parameter to shadow it,
    /// the outer alias is still what the annotation resolves to. This one passes before the
    /// change as well as after — it is the guard, not the driver.
    #[test]
    fn without_a_type_parameter_a_method_signature_kind_reads_the_outer_alias() {
        for source in [
            "type A = number;\ninterface I { m(x: A): A }",
            "type A = number;\nabstract class C { abstract m(x: A): A }",
        ] {
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_alias_declaration".to_owned()),
                "{source}"
            );
        }
    }

    // --- #208's remaining four: signature and type-position carriers --------------------
    //
    // Separate from the two above because they ship in their own commit. `call_signature`
    // and `construct_signature` are members of a body; `constructor_type` and
    // `function_type` are pure type position, which is untested ground for a walk that had
    // only ever descended declarations.

    #[test]
    fn a_type_parameter_binds_in_signature_or_type_position() {
        for source in [
            "type A = number;\ninterface F { <A>(x: A): A }",
            "type A = number;\ninterface F { new <A>(x: A): A }",
            "type A = number;\ntype F = new <A>(x: A) => A;",
            "type A = number;\ntype F = <A>(x: A) => A;",
        ] {
            assert_eq!(
                resolve_use(source, "A"),
                Some(Binding::Local(BindingKind::TypeParam)),
                "{source}"
            );
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_parameter".to_owned()),
                "{source}"
            );
        }
    }

    #[test]
    fn a_type_parameter_in_signature_or_type_position_shadows_an_outer_alias() {
        for source in [
            "type A = number;\ninterface F { <A>(x: A): A }",
            "type A = number;\ninterface F { new <A>(x: A): A }",
            "type A = number;\ntype F = new <A>(x: A) => A;",
            "type A = number;\ntype F = <A>(x: A) => A;",
        ] {
            assert!(shadowed(source, "A"), "{source}");
        }
    }

    /// The guard: passes before the change as well as after.
    #[test]
    fn without_a_type_parameter_signature_or_type_position_reads_the_outer_alias() {
        for source in [
            "type A = number;\ninterface F { (x: A): A }",
            "type A = number;\ninterface F { new (x: A): A }",
            "type A = number;\ntype F = new (x: A) => A;",
            "type A = number;\ntype F = (x: A) => A;",
        ] {
            assert_eq!(
                declaration_use(source, "A"),
                Some("type_alias_declaration".to_owned()),
                "{source}"
            );
        }
    }

    #[test]
    fn sibling_scopes_do_not_shadow_each_other() {
        // Two functions each declaring `t` are not shadowing anything — only nesting is.
        assert!(!shadowed(
            "function a() { const t = 1; return t; }\nfunction b() { const t = 2; return t; }",
            "t"
        ));
    }

    // --- declaration_of ----------------------------------------------------------------

    #[test]
    fn a_use_reaches_the_declarator_that_declares_it() {
        assert_eq!(
            declaration_use("const amount = 1;\nconst b = amount;\n", "amount"),
            Some("variable_declarator".to_owned())
        );
    }

    #[test]
    fn a_use_reaches_its_parameter_rather_than_the_parameter_list() {
        // The whole `formal_parameters` node would be useless to a caller reading a
        // `type` annotation: the annotation belongs to one parameter, not to the list.
        assert_eq!(
            declaration_use("function f(amount: number) { return amount; }", "amount"),
            Some("required_parameter".to_owned())
        );
    }

    #[test]
    fn an_optional_parameter_is_reached_as_itself() {
        assert_eq!(
            declaration_use("function f(amount?: number) { return amount; }", "amount"),
            Some("optional_parameter".to_owned())
        );
    }

    #[test]
    fn a_type_alias_is_reached_as_its_declaration() {
        assert_eq!(
            declaration_use("type Amount = number;\nlet x: Amount;", "Amount"),
            Some("type_alias_declaration".to_owned())
        );
    }

    #[test]
    fn a_use_reaches_the_import_that_bound_it() {
        assert_eq!(
            declaration_use("import { D } from 'd';\nconst x = D;\n", "D"),
            Some("import_statement".to_owned())
        );
    }

    #[test]
    fn a_name_nothing_declares_reaches_no_declaration() {
        assert_eq!(declaration_use("const x = missing;\n", "missing"), None);
    }

    /// The two projections agree about *whether* a name is declared.
    ///
    /// They share one walk, so this is a guard against that stopping being true — a
    /// node-returning path that silently found nothing would make every type answer
    /// `None`, which reads exactly like a file with nothing to say.
    #[test]
    fn the_two_projections_agree_on_presence() {
        for (source, name) in [
            ("const a = 1;\nconst b = a;\n", "a"),
            ("function f(p: number) { return p; }", "p"),
            ("import { D } from 'd';\nconst x = D;\n", "D"),
            ("const x = missing;\n", "missing"),
            ("function f<T>(x: T) { return x; }", "T"),
            ("for (const x of ns) { g(x); }", "x"),
            ("for (x of ns) { g(x); }", "x"),
        ] {
            assert_eq!(
                resolve_use(source, name).is_some(),
                declaration_use(source, name).is_some(),
                "disagreement for `{name}` in `{source}`"
            );
        }
    }

    /// Every kind the grammar declares `type_parameters` on is a scope, read off the grammar's
    /// own declaration rather than off a sample.
    ///
    /// AGENTS.md's entry on `SCOPE_KINDS` carried four wrong counts of these kinds, every one
    /// from a hand-written sample that did not parse everything, and a fifth nearly shipped
    /// with the change that closed the set. `node-types.json` is where the field is declared,
    /// so this reads it — both grammars, which declare the same kinds — and pins the two facts
    /// the entry states: which kinds carry the field, and which of those also carry
    /// `parameters`, the ones where scoping the kind also widened parameter resolution. A
    /// grammar bump that adds a carrier fails here instead of typing a type parameter as its
    /// outer alias in silence.
    #[test]
    fn every_carrier_the_grammar_declares_is_a_scope() {
        use std::collections::BTreeMap;

        // kind → whether it also carries `parameters`
        fn carriers(node_types: &str) -> BTreeMap<String, bool> {
            let kinds: Vec<serde_json::Value> =
                serde_json::from_str(node_types).expect("node-types.json parses");
            kinds
                .iter()
                .filter_map(|kind| {
                    let fields = kind.get("fields")?.as_object()?;
                    fields.contains_key("type_parameters").then(|| {
                        let name = kind["type"].as_str().expect("a kind has a type");
                        (name.to_owned(), fields.contains_key("parameters"))
                    })
                })
                .collect()
        }

        let typescript = carriers(tree_sitter_typescript::TYPESCRIPT_NODE_TYPES);
        let tsx = carriers(tree_sitter_typescript::TSX_NODE_TYPES);
        assert_eq!(
            typescript, tsx,
            "the two grammars declare the field on the same kinds"
        );
        assert_eq!(typescript.len(), 18, "the count AGENTS.md states");

        let missing: Vec<&str> = typescript
            .keys()
            .map(String::as_str)
            .filter(|kind| !SCOPE_KINDS.contains(kind))
            .collect();
        assert!(
            missing.is_empty(),
            "kinds carrying `type_parameters` that are not scopes: {missing:?}"
        );

        let without_parameters: Vec<&str> = typescript
            .iter()
            .filter(|(_, has_parameters)| !**has_parameters)
            .map(|(kind, _)| kind.as_str())
            .collect();
        assert_eq!(
            without_parameters,
            [
                "abstract_class_declaration",
                "class",
                "class_declaration",
                "interface_declaration",
                "type_alias_declaration",
            ],
            "the carriers AGENTS.md says declare no `parameters`"
        );
    }

    // --- #229: declaration kinds the resolver did not bind --------------------------------

    #[test]
    fn resolves_an_interface_declaration() {
        assert_eq!(
            resolve_use(
                "interface Amountish { m(): void }\nlet x: Amountish;",
                "Amountish"
            ),
            Some(Binding::Local(BindingKind::Type))
        );
    }

    #[test]
    fn resolves_an_enum_declaration() {
        assert_eq!(
            resolve_use("enum Color { Red }\nlet c: Color;", "Color"),
            Some(Binding::Local(BindingKind::Type))
        );
    }

    #[test]
    fn resolves_an_abstract_class_declaration() {
        assert_eq!(
            resolve_use(
                "abstract class Repo { abstract get(): number }\nlet r: Repo;",
                "Repo"
            ),
            Some(Binding::Local(BindingKind::Class))
        );
    }

    #[test]
    fn resolves_an_ambient_function_signature() {
        assert_eq!(
            resolve_use(
                "declare function credit(amount: number): void;\ncredit(1);",
                "credit"
            ),
            Some(Binding::Local(BindingKind::Function))
        );
    }

    #[test]
    fn resolves_an_internal_module_by_name() {
        assert_eq!(
            resolve_use(
                "namespace Payments { export const rate = 1 }\nlet r = Payments.rate;",
                "Payments"
            ),
            Some(Binding::Local(BindingKind::Module))
        );
    }

    #[test]
    fn resolves_a_module_declaration_with_an_identifier_name() {
        assert_eq!(
            resolve_use(
                "declare module Payments { export const rate = 1 }\nlet r = Payments.rate;",
                "Payments"
            ),
            Some(Binding::Local(BindingKind::Module))
        );
    }

    #[test]
    fn resolves_through_an_ambient_declaration_wrapper() {
        assert_eq!(
            resolve_use("declare const rate: number;\nlet x = rate;", "rate"),
            Some(Binding::Local(BindingKind::Const))
        );
    }

    #[test]
    fn an_interface_declaration_answers_the_interface_node() {
        assert_eq!(
            declaration_use(
                "interface Amountish { m(): void }\nlet x: Amountish;",
                "Amountish"
            ),
            Some("interface_declaration".to_owned())
        );
    }

    #[test]
    fn declares_answers_nothing_for_an_import() {
        let source = "import { Big } from './big';";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let statement = tree.root_node().named_child(0).expect("one statement");
        assert_eq!(statement.kind(), "import_statement");
        assert_eq!(JsBindingResolver.declares(source, statement, "Big"), None);
    }

    // --- #232 review: arms that bound the wrong name, or none at all ----------------------

    /// `namespace A.B {}` is shorthand for `namespace A { namespace B {} }`: `A` is the name
    /// the enclosing scope sees, and the dotted spelling is no binding at all.
    #[test]
    fn resolves_a_dotted_namespace_by_its_first_segment() {
        assert_eq!(
            resolve_use(
                "namespace Payments.Rates { export const rate = 1 }\nlet r = Payments.Rates.rate;",
                "Payments"
            ),
            Some(Binding::Local(BindingKind::Module))
        );
    }

    /// The grammar aliases every inner level of a dotted name to `member_expression`, so
    /// `google.maps.places` is a `nested_identifier` whose `object` is the member expression
    /// `google.maps` — one more level than a two-segment fixture ever exercises.
    #[test]
    fn resolves_a_three_segment_namespace_by_its_first_segment() {
        assert_eq!(
            resolve_use(
                "declare namespace google.maps.places { const q: number }\nlet g = google;",
                "google"
            ),
            Some(Binding::Local(BindingKind::Module))
        );
        assert_eq!(
            declares_at_top("namespace google.maps.places {}", "google.maps"),
            None
        );
    }

    #[test]
    fn a_dotted_namespace_does_not_bind_its_own_spelling() {
        assert_eq!(
            declares_at_top("namespace A.B { export const q = 1 }", "A.B"),
            None
        );
        assert_eq!(
            declares_at_top("namespace A.B { export const q = 1 }", "A"),
            Some("internal_module".to_owned())
        );
    }

    /// A comment is a named extra, so `named_child(0)` of an `ambient_declaration` is the
    /// comment rather than the declaration `declare` applies to.
    #[test]
    fn resolves_through_an_ambient_declaration_carrying_a_comment() {
        assert_eq!(
            resolve_use("declare /* c */ const rate: number;\nlet x = rate;", "rate"),
            Some(Binding::Local(BindingKind::Const))
        );
        assert_eq!(
            resolve_use("declare /** doc */ namespace N {}\nlet x = N;", "N"),
            Some(Binding::Local(BindingKind::Module))
        );
    }

    /// The question `lanekeep-types` asks of every top-level statement, reached the way that
    /// crate reaches it: through the trait, with no knowledge of which resolver answers.
    #[test]
    fn declares_is_reachable_through_the_trait() {
        let source = "export declare class Big {}\nimport { Small } from './small';";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let root = tree.root_node();
        let resolver: &dyn BindingResolver = &JsBindingResolver;
        let class = root.named_child(0).expect("the export");
        let import = root.named_child(1).expect("the import");
        assert_eq!(
            resolver
                .declares(source, class, "Big")
                .map(|node| node.kind().to_owned()),
            Some("class_declaration".to_owned())
        );
        assert_eq!(resolver.declares(source, import, "Small"), None);
    }

    /// A resolver that answers the two required questions and nothing else.
    struct Bare;

    impl BindingResolver for Bare {
        fn resolve(&self, _tree: &Tree, _source: &str, _node: Node<'_>) -> Option<Binding> {
            None
        }

        fn is_shadowed(&self, _tree: &Tree, _source: &str, _node: Node<'_>) -> bool {
            false
        }
    }

    /// The trait's default is silence, on the same terms as `declaration_of`: a language
    /// whose resolver cannot yet say which statement declares a name hands back nothing
    /// rather than a guess.
    #[test]
    fn declares_defaults_to_nothing() {
        let source = "const x = 1;";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let statement = tree.root_node().named_child(0).expect("one statement");
        assert_eq!(Bare.declares(source, statement, "x"), None);
    }

    /// `declares` on the first top-level statement, projected to the kind of the node it
    /// answers with.
    fn declares_at_top(source: &str, name: &str) -> Option<String> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");
        let statement = tree.root_node().named_child(0).expect("one statement");
        JsBindingResolver
            .declares(source, statement, name)
            .map(|node| node.kind().to_owned())
    }
}
