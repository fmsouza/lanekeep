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
        let mut cursor = scope.walk();

        // Parameters belong to the function, not to its body block.
        if let Some(params) = scope.child_by_field_name("parameters")
            && pattern_binds(params, source, name)
        {
            return Some(Binding::Local(BindingKind::Param));
        }
        // An arrow function with a single unparenthesized parameter.
        if scope.kind() == "arrow_function"
            && let Some(parameter) = scope.child_by_field_name("parameter")
            && node_text(parameter, source) == name
        {
            return Some(Binding::Local(BindingKind::Param));
        }
        if scope.kind() == "catch_clause"
            && let Some(parameter) = scope.child_by_field_name("parameter")
            && pattern_binds(parameter, source, name)
        {
            return Some(Binding::Local(BindingKind::CatchParam));
        }

        for child in scope.children(&mut cursor) {
            if let Some(binding) = declaration_binding(child, source, name) {
                return Some(binding);
            }
        }
        None
    }
}

/// What `name` is bound to by this statement, if anything.
fn declaration_binding(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    match node.kind() {
        "import_statement" => import_binding(node, source, name),

        "lexical_declaration" | "variable_declaration" => {
            let kind = declaration_kind(node, source);
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
                .filter_map(|declarator| declarator.child_by_field_name("name"))
                .any(|pattern| pattern_binds(pattern, source, name))
                .then_some(Binding::Local(kind))
        }

        "function_declaration" | "generator_function_declaration" => {
            named_as(node, source, name).then_some(Binding::Local(BindingKind::Function))
        }

        "class_declaration" => {
            named_as(node, source, name).then_some(Binding::Local(BindingKind::Class))
        }

        // `export const x = 1`, `export function f() {}` — the declaration is inside.
        "export_statement" => node
            .child_by_field_name("declaration")
            .and_then(|inner| declaration_binding(inner, source, name)),

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

    fn with_use<T>(source: &str, name: &str, f: impl Fn(&Tree, Node<'_>) -> T) -> T {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");

        // The last occurrence *by position*, not by traversal order — the traversal is
        // depth-first over a stack, so "last visited" is not "last in the file", and the
        // tests mean the latter.
        let mut found: Option<Node<'_>> = None;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "identifier"
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
}
