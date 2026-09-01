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

        "function_declaration" | "generator_function_declaration" => {
            named_as(node, source, name).then_some((node, Binding::Local(BindingKind::Function)))
        }

        "class_declaration" => {
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
        "type_alias_declaration" => {
            named_as(node, source, name).then_some((node, Binding::Local(BindingKind::Type)))
        }

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

/// `const` or `let` for a lexical declaration, `var` otherwise.
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
                        .map(|n| node_text(n, source).to_owned())?;
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
        ] {
            assert_eq!(
                resolve_use(source, name).is_some(),
                declaration_use(source, name).is_some(),
                "disagreement for `{name}` in `{source}`"
            );
        }
    }
}
