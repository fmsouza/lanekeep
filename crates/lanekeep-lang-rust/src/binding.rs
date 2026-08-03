//! Syntactic binding resolution for Rust.
//!
//! Rust is the fourth language here and the one whose *patterns* do the most work. The other
//! three bind a name by writing it; Rust binds several at once through destructuring, and the
//! shape doing it also mentions names that are emphatically not bindings.
//!
//! **A pattern's type is not a binding.** `let Some(v) = opt` parses as a `tuple_struct_pattern`
//! whose `type` field is the identifier `Some` and whose child is the identifier `v`. Only `v`
//! is bound. Walking the pattern naively makes every `Some`, `Ok` and `Err` in a file resolve
//! to a local variable, which is worse than resolving nothing: a rule asking "is this the
//! imported `Result`?" starts answering no everywhere.
//!
//! **Items are order-independent.** A function may call one declared below it, and a `const`
//! may reference a type declared later, so a module's scope is scanned whole rather than up to
//! the use — the same property Go has and JavaScript does not.
//!
//! What is deliberately not modeled is *ordering within a block*, and macros. `let x = x;`
//! reads the outer `x` on the right, and resolution here does not distinguish that; a program
//! where it matters does not compile. Macro bodies are not expanded — `macro_rules!` contents
//! are token trees rather than expressions, and pretending to resolve inside one would invent
//! bindings that may never exist.

use lanekeep_lang::binding::{Binding, BindingKind, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

/// Resolves Rust identifiers to the declaration that introduced them.
#[derive(Debug, Clone, Copy, Default)]
pub struct RustBindingResolver;

/// Node kinds that introduce a scope.
const SCOPE_KINDS: &[&str] = &[
    "source_file",
    "function_item",
    "function_signature_item",
    "closure_expression",
    "block",
    "mod_item",
    "impl_item",
    "trait_item",
    // Each binds through a pattern that outlives its own header: `if let Some(v) = x` reaches
    // the consequence, `for item in xs` reaches the body, a match arm reaches its value.
    "if_expression",
    "while_expression",
    "for_expression",
    "match_arm",
];

/// Identifier kinds that can refer to a binding.
///
/// Rust spells the same reference differently by position: a type is a `type_identifier`, a
/// value an `identifier`, and a field shorthand in a pattern a `shorthand_field_identifier`.
const REFERENCE_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "shorthand_field_identifier",
];

impl RustBindingResolver {
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
        match scope.kind() {
            // A signature binds its parameters and generics but has no body to scan.
            "function_item" | "function_signature_item" => signature_binds(scope, source, name),

            "closure_expression" => scope
                .child_by_field_name("parameters")
                .and_then(|parameters| parameter_list_binds(parameters, source, name)),

            // The header's pattern reaches the whole expression, including the else branch.
            "if_expression" | "while_expression" => scope
                .child_by_field_name("condition")
                .filter(|condition| condition.kind() == "let_condition")
                .and_then(|condition| condition.child_by_field_name("pattern"))
                .filter(|pattern| pattern_binds(*pattern, source, name))
                .map(|_| Binding::Local(BindingKind::Let)),

            "for_expression" => scope
                .child_by_field_name("pattern")
                .filter(|pattern| pattern_binds(*pattern, source, name))
                .map(|_| Binding::Local(BindingKind::Loop)),

            "match_arm" => scope
                .child_by_field_name("pattern")
                .filter(|pattern| pattern_binds(*pattern, source, name))
                .map(|_| Binding::Local(BindingKind::Let)),

            // `impl`, `trait` and `mod` hold their items in a `declaration_list`; a bare
            // `mod parser;` has no body at all.
            "mod_item" | "impl_item" | "trait_item" => scope
                .child_by_field_name("body")
                .and_then(|body| items_bind(body, source, name))
                .or_else(|| generics_bind(scope, source, name)),

            // `source_file` and `block`, which scan their own direct children — a module's
            // items in any order, and a block's statements.
            _ => items_bind(scope, source, name),
        }
    }
}

impl BindingResolver for RustBindingResolver {
    fn resolve(&self, _tree: &Tree, source: &str, node: Node<'_>) -> Option<Binding> {
        if !REFERENCE_KINDS.contains(&node.kind()) {
            return None;
        }
        let name = node_text(node, source);

        for scope in Self::scopes(node) {
            if let Some(binding) = Self::declaration_in(scope, source, name) {
                return Some(binding);
            }
        }
        None
    }

    fn is_shadowed(&self, _tree: &Tree, source: &str, node: Node<'_>) -> bool {
        if !REFERENCE_KINDS.contains(&node.kind()) {
            return false;
        }
        let name = node_text(node, source);

        Self::scopes(node)
            .into_iter()
            .filter(|scope| Self::declaration_in(*scope, source, name).is_some())
            .count()
            > 1
    }
}

/// Scan a node's direct children for a declaration of `name`.
fn items_bind(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    node.named_children(&mut node.walk())
        .find_map(|child| declares(child, source, name))
}

/// Whether this node is a declaration of `name`, and of what kind.
fn declares(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    match node.kind() {
        "use_declaration" => use_binds(node, source, name),

        "let_declaration" => node
            .child_by_field_name("pattern")
            .filter(|pattern| pattern_binds(*pattern, source, name))
            .map(|_| Binding::Local(BindingKind::Let)),

        // `static` is a variable that outlives everything, which is what `var` says here.
        // `const` has its own kind because Rust draws the same line the word does.
        "const_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Const))
        }
        "static_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Var))
        }

        "function_item" | "function_signature_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Function))
        }

        // A struct, enum, union or alias all name a type. A trait does not — it names a
        // bound, and a rule about traits wants to say so.
        "struct_item" | "enum_item" | "union_item" | "type_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Type))
        }
        "trait_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Trait))
        }
        "mod_item" => {
            named_binds(node, source, name).then_some(Binding::Local(BindingKind::Module))
        }

        _ => None,
    }
}

/// Whether a declaration's `name` field is `name`.
fn named_binds(node: Node<'_>, source: &str, name: &str) -> bool {
    node.child_by_field_name("name")
        .is_some_and(|declared| node_text(declared, source) == name)
}

/// Whether a pattern binds `name`.
///
/// The whole of the difficulty in this file. A pattern is a tree that mixes names being bound
/// with names being *matched against*, and only the first are bindings.
/// There is deliberately no guard for `_` here, unlike the Go resolver. Go's blank identifier
/// parses as an ordinary `identifier` and has to be excluded by name; Rust's wildcard is not an
/// identifier at all — `let _ = x` has no `pattern` field, and a `_` match arm is a leaf with no
/// named children. A guard would be unreachable, and an unreachable guard implies a case that
/// does not exist.
fn pattern_binds(pattern: Node<'_>, source: &str, name: &str) -> bool {
    match pattern.kind() {
        // `let Point { x, y } = p` binds both through the shorthand.
        "identifier" | "shorthand_field_identifier" => node_text(pattern, source) == name,

        // The constructor being matched is not a binding. `let Some(v) = opt` binds `v`;
        // treating `Some` as a binding makes every constructor in the file resolve to a
        // local, and a rule asking whether a name is an import starts answering no.
        "tuple_struct_pattern" | "struct_pattern" => {
            let constructor = pattern.child_by_field_name("type").map(|c| c.id());
            pattern
                .named_children(&mut pattern.walk())
                .filter(|child| Some(child.id()) != constructor)
                .any(|child| pattern_binds(child, source, name))
        }

        // Every other pattern shape is a container: tuples, slices, references, `or`
        // alternatives, struct fields, `mut`/`ref` bindings, ranges.
        _ => pattern
            .named_children(&mut pattern.walk())
            .any(|child| pattern_binds(child, source, name)),
    }
}

/// A function's parameters and generics.
fn signature_binds(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    if let Some(found) = generics_bind(scope, source, name) {
        return Some(found);
    }
    scope
        .child_by_field_name("parameters")
        .and_then(|parameters| parameter_list_binds(parameters, source, name))
}

/// Generic parameters, on anything that can carry them.
fn generics_bind(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    scope
        .child_by_field_name("type_parameters")
        .filter(|parameters| {
            parameters
                .named_children(&mut parameters.walk())
                .any(|declaration| named_binds(declaration, source, name))
        })
        .map(|_| Binding::Local(BindingKind::TypeParam))
}

/// Whether a parameter list binds `name`, through any of its patterns.
fn parameter_list_binds(list: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    list.named_children(&mut list.walk())
        .filter(|parameter| parameter.kind() == "parameter")
        .filter_map(|parameter| parameter.child_by_field_name("pattern"))
        .any(|pattern| pattern_binds(pattern, source, name))
        .then_some(Binding::Local(BindingKind::Param))
}

/// Whether a `use` declaration binds `name`, and to which path.
fn use_binds(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    let argument = node.child_by_field_name("argument")?;
    use_tree_binds(argument, source, name, "")
}

/// Walk a use tree, carrying the path accumulated so far.
fn use_tree_binds(node: Node<'_>, source: &str, name: &str, prefix: &str) -> Option<Binding> {
    match node.kind() {
        // `use foo;`
        "identifier" | "type_identifier" => {
            (node_text(node, source) == name).then(|| import_of(prefix, node_text(node, source)))
        }

        // `use std::collections::HashMap;` — the last segment is the bound name, the rest
        // is the path.
        "scoped_identifier" => {
            let declared = node.child_by_field_name("name")?;
            (node_text(declared, source) == name).then(|| {
                let path = node
                    .child_by_field_name("path")
                    .map_or_else(String::new, |p| node_text(p, source).to_owned());
                import_of(&path, node_text(declared, source))
            })
        }

        // `use std::io::{Read, Write as W};`
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map_or_else(String::new, |p| node_text(p, source).to_owned());
            let list = node.child_by_field_name("list")?;
            list.named_children(&mut list.walk())
                .find_map(|entry| use_tree_binds(entry, source, name, &path))
        }
        "use_list" => node
            .named_children(&mut node.walk())
            .find_map(|entry| use_tree_binds(entry, source, name, prefix)),

        // `use foo::Bar as Baz;` — only the alias is in scope.
        "use_as_clause" => {
            let alias = node.child_by_field_name("alias")?;
            (node_text(alias, source) == name).then(|| {
                let original = node
                    .child_by_field_name("path")
                    .map_or("", |p| node_text(p, source));
                // The path recorded is what was imported, not what it was renamed to — a
                // rule matching on the module has to see through the alias.
                import_of(prefix, original.rsplit("::").next().unwrap_or(original))
            })
        }

        // Nothing else in a use tree binds a name. `use_wildcard` is the one worth naming:
        // `use serde::*;` brings in names that cannot be known without reading the crate, so
        // nothing is claimed rather than guessed — which is exactly why `no-glob-import`
        // exists as a rule.
        _ => None,
    }
}

fn import_of(path: &str, name: &str) -> Binding {
    Binding::Import {
        module: path.to_owned(),
        name: ImportedName::Named(name.to_owned()),
    }
}

fn node_text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;

    use super::*;
    use crate::Rust;

    /// Resolve the last identifier in the source that reads exactly `name`.
    ///
    /// Rules resolve a *use*, so tests must too — resolving the declaration site exercises a
    /// different path than the one that matters.
    fn resolve_use(source: &str, name: &str) -> Option<Binding> {
        with_use(source, name, |tree, node| {
            RustBindingResolver.resolve(tree, source, node)
        })
    }

    fn shadowed(source: &str, name: &str) -> bool {
        with_use(source, name, |tree, node| {
            RustBindingResolver.is_shadowed(tree, source, node)
        })
    }

    fn with_use<T>(source: &str, name: &str, f: impl Fn(&Tree, Node<'_>) -> T) -> T {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&Rust.grammar()).expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");

        // The last occurrence *by position*: the traversal is a depth-first stack, so "last
        // visited" is not "last in the file", and the tests mean the latter.
        let mut found: Option<Node<'_>> = None;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if REFERENCE_KINDS.contains(&node.kind())
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

    fn imported(path: &str, name: &str) -> Binding {
        Binding::Import {
            module: path.to_owned(),
            name: ImportedName::Named(name.to_owned()),
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Option is the point: it matches what resolve_use returns, and \
                  unwrapping it at every call site would bury the assertion"
    )]
    fn local(kind: BindingKind) -> Option<Binding> {
        Some(Binding::Local(kind))
    }

    // --- use declarations --------------------------------------------------------------

    #[test]
    fn a_scoped_use_binds_its_last_segment() {
        assert_eq!(
            resolve_use(
                "use std::collections::HashMap;\nfn f() { let _: HashMap<u8, u8>; }\n",
                "HashMap"
            ),
            Some(imported("std::collections", "HashMap"))
        );
    }

    #[test]
    fn a_use_list_binds_every_entry() {
        let source = "use std::io::{Read, Write};\nfn f(r: Read, w: Write) {}\n";
        assert_eq!(
            resolve_use(source, "Read"),
            Some(imported("std::io", "Read"))
        );
        assert_eq!(
            resolve_use(source, "Write"),
            Some(imported("std::io", "Write"))
        );
    }

    #[test]
    fn an_alias_binds_only_the_new_name() {
        // The point of `as` is that the original is not in scope.
        let source = "use std::io::{Write as W};\nfn f(w: W) {}\n";
        assert_eq!(resolve_use(source, "W"), Some(imported("std::io", "Write")));
        assert_eq!(
            resolve_use(
                "use std::io::Write as W;\nfn f(x: u8) { let Write = x; }\n",
                "Write"
            ),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn a_glob_import_claims_nothing() {
        // Which names a glob brings in cannot be known without reading the crate, so nothing
        // is claimed rather than guessed at.
        assert_eq!(
            resolve_use(
                "use serde::*;\nfn f() { let _ = Serialize; }\n",
                "Serialize"
            ),
            None
        );
    }

    // --- items are order-independent ----------------------------------------------------

    #[test]
    fn an_item_is_visible_before_it_is_written() {
        // Rust's module scope is order-independent, which is the property that makes this
        // resolver different from the JavaScript one rather than a copy of it.
        assert_eq!(
            resolve_use("fn f() { let _ = MAX; }\nconst MAX: u8 = 3;\n", "MAX"),
            local(BindingKind::Const)
        );
    }

    #[test]
    fn each_item_kind_reports_itself() {
        for (source, name, kind) in [
            (
                "struct Repo;\nfn f(r: Repo) {}\n",
                "Repo",
                BindingKind::Type,
            ),
            (
                "enum Kind { A }\nfn f(k: Kind) {}\n",
                "Kind",
                BindingKind::Type,
            ),
            (
                "type Alias = u8;\nfn f(a: Alias) {}\n",
                "Alias",
                BindingKind::Type,
            ),
            (
                "trait Store {}\nfn f<T: Store>(t: T) {}\n",
                "Store",
                BindingKind::Trait,
            ),
            (
                "mod parser;\nfn f() { parser::go(); }\n",
                "parser",
                BindingKind::Module,
            ),
            (
                "static NAME: u8 = 1;\nfn f() { let _ = NAME; }\n",
                "NAME",
                BindingKind::Var,
            ),
            (
                "const MAX: u8 = 1;\nfn f() { let _ = MAX; }\n",
                "MAX",
                BindingKind::Const,
            ),
            (
                "fn helper() {}\nfn f() { helper(); }\n",
                "helper",
                BindingKind::Function,
            ),
        ] {
            assert_eq!(resolve_use(source, name), local(kind), "{name}");
        }
    }

    // --- patterns -------------------------------------------------------------------------

    #[test]
    fn a_let_binds_a_plain_name() {
        assert_eq!(
            resolve_use("fn f() {\n    let x = 1;\n    let _ = x;\n}\n", "x"),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn a_destructuring_let_binds_every_name() {
        for (source, name) in [
            (
                "fn f(p: (u8, u8)) {\n    let (a, b) = p;\n    let _ = a;\n}\n",
                "a",
            ),
            (
                "fn f(p: (u8, u8)) {\n    let (a, b) = p;\n    let _ = b;\n}\n",
                "b",
            ),
            (
                "fn f(p: P) {\n    let P { x, y } = p;\n    let _ = x;\n}\n",
                "x",
            ),
            (
                "fn f(p: [u8; 2]) {\n    let [c, d] = p;\n    let _ = d;\n}\n",
                "d",
            ),
        ] {
            assert_eq!(resolve_use(source, name), local(BindingKind::Let), "{name}");
        }
    }

    #[test]
    fn a_pattern_constructor_is_not_a_binding() {
        // The trap this resolver exists to avoid. `Some` is matched against, not bound;
        // treating it as a binding makes every constructor in a file resolve to a local, and
        // a rule asking whether a name is an import starts answering no everywhere.
        let source = "use core::option::Option::Some;\nfn f(o: Option<u8>) {\n    if let Some(v) = o { let _ = v; }\n}\n";
        assert_eq!(
            resolve_use(source, "Some"),
            Some(imported("core::option::Option", "Some")),
            "Some is the import, not the pattern's binding"
        );
        assert_eq!(resolve_use(source, "v"), local(BindingKind::Let));
    }

    #[test]
    fn a_struct_pattern_type_is_not_a_binding_either() {
        let source = "struct Point { x: u8, y: u8 }\nfn f(p: Point) {\n    let Point { x, y } = p;\n    let _ = x;\n}\n";
        assert_eq!(resolve_use(source, "Point"), local(BindingKind::Type));
    }

    #[test]
    fn an_if_let_binding_reaches_the_else_branch() {
        // Rust scopes it across the whole expression.
        assert_eq!(
            resolve_use(
                "fn f(o: Option<u8>) {\n    if let Some(v) = o {} else { let _ = v; }\n}\n",
                "v"
            ),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn a_match_arm_binds_within_its_own_arm() {
        assert_eq!(
            resolve_use(
                "fn f(v: u8) {\n    match v { got => { let _ = got; } }\n}\n",
                "got"
            ),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn an_or_pattern_binds_through_both_alternatives() {
        assert_eq!(
            resolve_use(
                "fn f(r: R) {\n    match r { Ok(n) | Err(n) => { let _ = n; } }\n}\n",
                "n"
            ),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn a_ref_pattern_still_binds() {
        assert_eq!(
            resolve_use(
                "fn f(o: Option<u8>) {\n    if let Some(ref e) = o { let _ = e; }\n}\n",
                "e"
            ),
            local(BindingKind::Let)
        );
    }

    #[test]
    fn a_for_pattern_is_a_loop_binding() {
        assert_eq!(
            resolve_use(
                "fn f() {\n    for item in 0..3 { let _ = item; }\n}\n",
                "item"
            ),
            local(BindingKind::Loop)
        );
    }

    #[test]
    fn a_while_let_binds_in_its_body() {
        assert_eq!(
            resolve_use(
                "fn f(mut it: I) {\n    while let Some(w) = it.next() { let _ = w; }\n}\n",
                "w"
            ),
            local(BindingKind::Let)
        );
    }

    // --- signatures -------------------------------------------------------------------------

    #[test]
    fn parameters_bind_including_destructured_ones() {
        assert_eq!(
            resolve_use("fn f(id: u8) { let _ = id; }\n", "id"),
            local(BindingKind::Param)
        );
        assert_eq!(
            resolve_use("fn f((a, b): (u8, u8)) { let _ = a; }\n", "a"),
            local(BindingKind::Param)
        );
    }

    #[test]
    fn a_closure_parameter_belongs_to_the_closure() {
        assert_eq!(
            resolve_use("fn f() {\n    let g = |arg: u8| arg + 1;\n}\n", "arg"),
            local(BindingKind::Param)
        );
    }

    #[test]
    fn type_and_const_generics_bind() {
        assert_eq!(
            resolve_use("fn g<T: Clone>(v: T) -> T { v }\n", "T"),
            local(BindingKind::TypeParam)
        );
        assert_eq!(
            resolve_use("fn g<const N: usize>() -> usize { N }\n", "N"),
            local(BindingKind::TypeParam)
        );
    }

    #[test]
    fn an_impl_generic_reaches_its_methods() {
        assert_eq!(
            resolve_use(
                "struct S<T>(T);\nimpl<T> S<T> {\n    fn get(&self) -> T { todo!() }\n}\n",
                "T"
            ),
            local(BindingKind::TypeParam)
        );
    }

    // --- scoping -----------------------------------------------------------------------------

    #[test]
    fn a_binding_in_a_nested_block_does_not_leak_outwards() {
        assert_eq!(
            resolve_use(
                "fn f() {\n    { let inner = 1; let _ = inner; }\n}\nfn g() { let _ = inner; }\n",
                "inner"
            ),
            None
        );
    }

    #[test]
    fn a_wildcard_let_binds_nothing() {
        // `let _ = compute;` discards. If the wildcard bound anything, the name on the right
        // would look declared and a rule asking where it came from would get the wrong answer.
        //
        // There is no assertion about resolving `_` itself, because there is no `_` node to
        // resolve: the grammar gives `let _ = 1` no `pattern` field at all.
        assert_eq!(
            resolve_use("fn f() {\n    let _ = compute;\n}\n", "compute"),
            None
        );
    }

    #[test]
    fn an_undeclared_name_resolves_to_nothing() {
        assert_eq!(
            resolve_use("fn f() { let _ = missing; }\n", "missing"),
            None
        );
    }

    #[test]
    fn a_local_shadowing_an_import_is_shadowed() {
        // The case that makes import-based rules wrong when it is missed.
        let source = "use std::collections::HashMap;\nfn f() {\n    let HashMap = 1;\n    let _ = HashMap;\n}\n";
        assert!(shadowed(source, "HashMap"));
        assert_eq!(resolve_use(source, "HashMap"), local(BindingKind::Let));
    }

    #[test]
    fn a_single_declaration_however_nested_is_not_shadowing() {
        assert!(!shadowed(
            "fn f() {\n    let x = 1;\n    let _ = x;\n}\n",
            "x"
        ));
    }

    #[test]
    fn a_non_identifier_node_resolves_to_nothing() {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&Rust.grammar()).expect("grammar loads");
        let source = "fn f() {}\n";
        let tree = parser.parse(source, None).expect("parses");
        let root = tree.root_node();
        assert_eq!(RustBindingResolver.resolve(&tree, source, root), None);
        assert!(!RustBindingResolver.is_shadowed(&tree, source, root));
    }
}
