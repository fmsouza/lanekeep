//! Binding resolution for Python.
//!
//! The same outward walk the JavaScript resolver uses — innermost enclosing scope that
//! declares the name wins — over a different set of scopes, because Python's differ from
//! JavaScript's in two ways that change the answer.
//!
//! **Python has no block scope.** `if`, `for`, `while` and `try` do not introduce one, so a
//! name bound inside an `if` is bound for the whole function. Searching a scope therefore
//! means walking its entire body, not its direct children, stopping only where a nested
//! scope begins. The JavaScript resolver can look at direct children because a block *is* a
//! scope there.
//!
//! **A class body is a scope that functions do not see through.** A method cannot read a
//! class-level name without `self` or the class, so a class scope is skipped when resolving
//! from inside a function nested in it. Treating it as an ordinary enclosing scope would
//! report a binding Python does not actually provide.
//!
//! Deliberately syntactic, on the same terms as §1: nothing here imports a module to see
//! what it exports, follows `__all__`, or knows a type. The two cases that matter are within
//! that line — an aliased import resolves to what it aliases, and a local binding is not
//! mistaken for an import of the same name.

use lanekeep_lang::binding::{Binding, BindingKind, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

/// Syntactic binding resolution for Python.
#[derive(Debug, Clone, Copy, Default)]
pub struct PythonBindingResolver;

/// Node kinds that introduce a scope.
///
/// Comprehensions are here because Python 3 scopes their targets: the `x` in
/// `[x for x in xs]` does not leak, and a rule asking about an outer `x` must not be given
/// the comprehension's.
const SCOPE_KINDS: &[&str] = &[
    "module",
    "function_definition",
    "lambda",
    "class_definition",
    "list_comprehension",
    "set_comprehension",
    "dictionary_comprehension",
    "generator_expression",
];

/// Scope kinds that a nested function does not see through.
const OPAQUE_TO_NESTED_FUNCTIONS: &[&str] = &["class_definition"];

impl PythonBindingResolver {
    /// Every enclosing scope, innermost first, skipping class bodies that a nested function
    /// cannot read through.
    fn scopes(node: Node<'_>) -> Vec<Node<'_>> {
        let mut out = Vec::new();
        let mut crossed_function = false;
        let mut current = node.parent();

        while let Some(scope) = current {
            if SCOPE_KINDS.contains(&scope.kind()) {
                // A class body is visible to code written directly in it and invisible to a
                // function defined inside it — `def m(self): return CONSTANT` does not see a
                // class-level `CONSTANT`.
                let opaque = crossed_function && OPAQUE_TO_NESTED_FUNCTIONS.contains(&scope.kind());
                if !opaque {
                    out.push(scope);
                }
                if matches!(scope.kind(), "function_definition" | "lambda") {
                    crossed_function = true;
                }
            }
            current = scope.parent();
        }
        out
    }

    /// The body of a scope, which is where its bindings live.
    fn body_of(scope: Node<'_>) -> Option<Node<'_>> {
        match scope.kind() {
            "module" => Some(scope),
            _ => scope.child_by_field_name("body"),
        }
    }

    /// What `name` is bound to in this scope, ignoring nested scopes.
    fn declaration_in(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
        // Parameters belong to the function itself rather than to its body.
        if matches!(scope.kind(), "function_definition" | "lambda")
            && let Some(parameters) = scope.child_by_field_name("parameters")
            && parameter_binds(parameters, source, name)
        {
            return Some(Binding::Local(BindingKind::Param));
        }

        // A comprehension's target is bound by its `for` clauses, which are siblings of the
        // body expression rather than children of a block.
        if (scope.kind().ends_with("comprehension") || scope.kind() == "generator_expression")
            && let Some(found) = comprehension_target(scope, source, name)
        {
            return Some(found);
        }

        // `def f` and `class C` bind their own name in the *enclosing* scope, so they are
        // found by walking the body below rather than by looking at the scope itself.
        let body = Self::body_of(scope)?;
        binding_in_subtree(body, source, name)
    }
}

impl BindingResolver for PythonBindingResolver {
    fn resolve(&self, _tree: &Tree, source: &str, node: Node<'_>) -> Option<Binding> {
        if node.kind() != "identifier" {
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
        if node.kind() != "identifier" {
            return false;
        }
        let name = node_text(node, source);

        // More than one enclosing scope declares it, and the innermost is the one `resolve`
        // returns. One declaration, however nested, is not shadowing.
        Self::scopes(node)
            .into_iter()
            .filter(|scope| Self::declaration_in(*scope, source, name).is_some())
            .count()
            > 1
    }
}

/// Search a subtree for a binding of `name`, without descending into a nested scope.
///
/// The nested-scope stop is what keeps `def inner(): x = 1` from binding `x` in the function
/// that contains `inner`. It does not apply to the subtree's own root, or searching a scope
/// body would stop immediately.
fn binding_in_subtree(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = binds_here(child, source, name) {
            return Some(found);
        }

        // `def` and `class` bind their name here and hide everything inside them.
        if SCOPE_KINDS.contains(&child.kind()) {
            continue;
        }

        if let Some(found) = binding_in_subtree(child, source, name) {
            return Some(found);
        }
    }
    None
}

/// Whether this node itself binds `name`, and how.
fn binds_here(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    match node.kind() {
        "function_definition" | "class_definition" => {
            let declared = node.child_by_field_name("name")?;
            (node_text(declared, source) == name).then(|| {
                Binding::Local(if node.kind() == "function_definition" {
                    BindingKind::Function
                } else {
                    BindingKind::Class
                })
            })
        }

        "import_statement" | "import_from_statement" => import_binding(node, source, name),

        "assignment" | "augmented_assignment" => {
            let target = node.child_by_field_name("left")?;
            target_binds(target, source, name).then_some(Binding::Local(BindingKind::Assignment))
        }

        // `x := 1`
        "named_expression" => {
            let target = node.child_by_field_name("name")?;
            (node_text(target, source) == name).then_some(Binding::Local(BindingKind::Assignment))
        }

        "for_statement" => {
            let target = node.child_by_field_name("left")?;
            target_binds(target, source, name).then_some(Binding::Local(BindingKind::Loop))
        }

        // `with open(p) as f` and `except E as e` bind the same way: an `as_pattern` whose
        // `as_pattern_target` holds the name. There is no `alias` field to reach it by.
        "with_item" => as_pattern_binds(node, source, name)
            .then_some(Binding::Local(BindingKind::ContextManager)),

        "except_clause" | "except_group_clause" => {
            as_pattern_binds(node, source, name).then_some(Binding::Local(BindingKind::CatchParam))
        }

        _ => None,
    }
}

/// The binding an `import` or `from ... import` introduces, if it introduces `name`.
///
/// `import a.b` binds `a`, not `b` — the whole dotted path is the module and the first
/// segment is the name that appears in scope.
fn import_binding(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    // In `from a.b import c`, both `a.b` and `c` are `dotted_name` nodes; only the field
    // tells them apart. Matching on kind alone reads the module as an imported name.
    let module_node = node.child_by_field_name("module_name");
    let from_module = module_node.map(|module| node_text(module, source).to_owned());

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if module_node.is_some_and(|module| module.id() == child.id()) {
            continue;
        }

        match child.kind() {
            "dotted_name" => {
                let text = node_text(child, source);
                match &from_module {
                    // `from m import a` — `a` is the export and `m` the module.
                    Some(module) => {
                        if text == name {
                            return Some(Binding::Import {
                                module: module.clone(),
                                name: ImportedName::Named(name.to_owned()),
                            });
                        }
                    }
                    // `import a.b` puts `a` in scope, not `b`, and what it names is the
                    // whole dotted path.
                    None => {
                        if text.split('.').next().unwrap_or(text) == name {
                            return Some(Binding::Import {
                                module: text.to_owned(),
                                name: ImportedName::Namespace,
                            });
                        }
                    }
                }
            }

            "aliased_import" => {
                let alias = child.child_by_field_name("alias")?;
                if node_text(alias, source) != name {
                    continue;
                }
                let original = child.child_by_field_name("name")?;
                let original = node_text(original, source);
                return Some(match &from_module {
                    // `from m import a as b` — the module is `m`, the export is `a`.
                    Some(module) => Binding::Import {
                        module: module.clone(),
                        name: ImportedName::Named(original.to_owned()),
                    },
                    // `import a.b as c` — `c` is the module itself.
                    None => Binding::Import {
                        module: original.to_owned(),
                        name: ImportedName::Namespace,
                    },
                });
            }

            // `from m import *` binds names this resolver cannot know without reading `m`,
            // which §1 puts out of scope. Reporting nothing is the honest answer.
            "wildcard_import" => return None,

            _ => {}
        }
    }
    None
}

/// Whether an `as` pattern under this node binds `name`.
///
/// Shared by `with ... as x` and `except E as x`, which have the same shape:
/// `as_pattern` → `as_pattern_target` → the name.
fn as_pattern_binds(node: Node<'_>, source: &str, name: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| {
        if child.kind() != "as_pattern" {
            return false;
        }
        let mut inner = child.walk();
        child.children(&mut inner).any(|part| {
            if part.kind() != "as_pattern_target" {
                return false;
            }
            let mut target = part.walk();
            part.children(&mut target)
                .any(|bound| target_binds(bound, source, name))
        })
    })
}

/// Whether an assignment or loop target binds `name`, including through tuple unpacking.
fn target_binds(target: Node<'_>, source: &str, name: &str) -> bool {
    match target.kind() {
        "identifier" => node_text(target, source) == name,

        // `a, b = ...`, `[a, b] = ...`, and `a, *rest = ...`.
        "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern" => {
            let mut cursor = target.walk();
            target
                .children(&mut cursor)
                .any(|child| target_binds(child, source, name))
        }

        // `obj.attr = 1` and `xs[0] = 1` assign through something already bound; neither
        // introduces a name.
        _ => false,
    }
}

/// A comprehension's `for` targets, which are scoped to the comprehension itself.
fn comprehension_target(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    let mut cursor = scope.walk();
    for child in scope.children(&mut cursor) {
        if child.kind() != "for_in_clause" {
            continue;
        }
        if let Some(target) = child.child_by_field_name("left")
            && target_binds(target, source, name)
        {
            return Some(Binding::Local(BindingKind::Comprehension));
        }
    }
    None
}

/// Whether a parameter list binds `name`, including defaults, `*args` and `**kwargs`.
fn parameter_binds(parameters: Node<'_>, source: &str, name: &str) -> bool {
    let mut cursor = parameters.walk();
    parameters.children(&mut cursor).any(|child| {
        match child.kind() {
            "identifier" => node_text(child, source) == name,
            // `x=1`, `x: int`, `x: int = 1`, `*args`, `**kwargs`.
            "default_parameter"
            | "typed_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => child.child_by_field_name("name").map_or_else(
                || {
                    let mut inner = child.walk();
                    child
                        .children(&mut inner)
                        .any(|part| part.kind() == "identifier" && node_text(part, source) == name)
                },
                |declared| node_text(declared, source) == name,
            ),
            _ => false,
        }
    })
}

fn node_text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    source.get(node.byte_range()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;

    use super::*;
    use crate::Python;

    /// Resolve the last identifier in the source that reads exactly `name`.
    ///
    /// Rules resolve a *use*, so tests must too — resolving the declaration site exercises a
    /// different path than the one that matters.
    fn resolve_use(source: &str, name: &str) -> Option<Binding> {
        with_use(source, name, |tree, node| {
            PythonBindingResolver.resolve(tree, source, node)
        })
    }

    fn shadowed(source: &str, name: &str) -> bool {
        with_use(source, name, |tree, node| {
            PythonBindingResolver.is_shadowed(tree, source, node)
        })
    }

    fn with_use<T>(source: &str, name: &str, f: impl Fn(&Tree, Node<'_>) -> T) -> T {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&Python.grammar())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");

        // The last occurrence *by position*: the traversal is a depth-first stack, so "last
        // visited" is not "last in the file", and the tests mean the latter.
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

    fn named(module: &str, name: &str) -> Binding {
        Binding::Import {
            module: module.to_owned(),
            name: ImportedName::Named(name.to_owned()),
        }
    }

    fn whole(module: &str) -> Binding {
        Binding::Import {
            module: module.to_owned(),
            name: ImportedName::Namespace,
        }
    }

    /// The `Some(Local(..))` a successful resolve returns, so assertions read as a
    /// comparison against `resolve_use` rather than as construction.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Option is the point: it matches what resolve_use returns, and \
                  unwrapping it at every call site would bury the assertion"
    )]
    fn local(kind: BindingKind) -> Option<Binding> {
        Some(Binding::Local(kind))
    }

    // --- imports ---------------------------------------------------------------------

    #[test]
    fn a_plain_import_binds_the_module() {
        assert_eq!(
            resolve_use("import os\nos.getcwd()\n", "os"),
            Some(whole("os"))
        );
    }

    #[test]
    fn a_dotted_import_binds_its_first_segment() {
        // `import a.b` puts `a` in scope, not `b`.
        assert_eq!(
            resolve_use("import os.path\nos.path.join()\n", "os"),
            Some(whole("os.path"))
        );
    }

    #[test]
    fn an_aliased_module_import_resolves_to_what_it_aliases() {
        assert_eq!(
            resolve_use("import numpy as np\nnp.array([])\n", "np"),
            Some(whole("numpy"))
        );
    }

    #[test]
    fn a_from_import_carries_the_module_and_the_export() {
        assert_eq!(
            resolve_use("from requests import get\nget(url)\n", "get"),
            Some(named("requests", "get"))
        );
    }

    #[test]
    fn an_aliased_from_import_resolves_to_the_original_name() {
        // The case that makes text matching wrong: `sub` reads nothing like `subprocess`.
        assert_eq!(
            resolve_use("from subprocess import run as sub\nsub(cmd)\n", "sub"),
            Some(named("subprocess", "run"))
        );
    }

    #[test]
    fn a_relative_import_keeps_the_dots() {
        assert_eq!(
            resolve_use("from .models import User\nUser()\n", "User"),
            Some(named(".models", "User"))
        );
    }

    #[test]
    fn a_star_import_binds_nothing_this_resolver_can_name() {
        // Knowing what `*` brought in means reading the other module, which §1 excludes.
        // Nothing is the honest answer; a guess would be confidently wrong.
        assert_eq!(
            resolve_use("from os.path import *\njoin(a, b)\n", "join"),
            None
        );
    }

    // --- local bindings ----------------------------------------------------------------

    #[test]
    fn an_assignment_binds() {
        assert_eq!(
            resolve_use("total = 1\nprint(total)\n", "total"),
            local(BindingKind::Assignment)
        );
    }

    #[test]
    fn a_def_and_a_class_bind_their_own_names() {
        assert_eq!(
            resolve_use("def handler():\n    pass\nhandler()\n", "handler"),
            local(BindingKind::Function)
        );
        assert_eq!(
            resolve_use("class Model:\n    pass\nModel()\n", "Model"),
            local(BindingKind::Class)
        );
    }

    #[test]
    fn parameters_bind_in_every_form() {
        for source in [
            "def f(conn):\n    return conn\n",
            "def f(conn=None):\n    return conn\n",
            "def f(conn: Conn):\n    return conn\n",
            "def f(conn: Conn = None):\n    return conn\n",
            "def f(*conn):\n    return conn\n",
            "def f(**conn):\n    return conn\n",
            "f = lambda conn: conn\n",
        ] {
            assert_eq!(
                resolve_use(source, "conn"),
                local(BindingKind::Param),
                "{source}"
            );
        }
    }

    #[test]
    fn a_loop_target_binds() {
        assert_eq!(
            resolve_use("for row in rows:\n    print(row)\n", "row"),
            local(BindingKind::Loop)
        );
    }

    #[test]
    fn tuple_unpacking_binds_every_name() {
        assert_eq!(
            resolve_use("key, value = pair\nprint(value)\n", "value"),
            local(BindingKind::Assignment)
        );
        assert_eq!(
            resolve_use("head, *rest = xs\nprint(rest)\n", "rest"),
            local(BindingKind::Assignment)
        );
    }

    #[test]
    fn a_context_manager_alias_binds() {
        assert_eq!(
            resolve_use("with open(p) as handle:\n    handle.read()\n", "handle"),
            local(BindingKind::ContextManager)
        );
    }

    #[test]
    fn an_except_alias_binds() {
        assert_eq!(
            resolve_use(
                "try:\n    go()\nexcept ValueError as err:\n    print(err)\n",
                "err"
            ),
            local(BindingKind::CatchParam)
        );
    }

    #[test]
    fn a_walrus_binds() {
        assert_eq!(
            resolve_use("if (found := lookup()):\n    print(found)\n", "found"),
            local(BindingKind::Assignment)
        );
    }

    #[test]
    fn assigning_through_an_attribute_or_subscript_binds_nothing() {
        // `config.debug = True` writes to something already bound; it introduces no name.
        assert_eq!(
            resolve_use("config.debug = True\nprint(debug)\n", "debug"),
            None
        );
        assert_eq!(resolve_use("xs[0] = 1\nprint(xs0)\n", "xs0"), None);
    }

    // --- scope, which is where Python differs -------------------------------------------

    #[test]
    fn python_has_no_block_scope() {
        // Bound inside an `if`, visible after it. A resolver that looked only at a scope's
        // direct children — which is correct for JavaScript — would miss this.
        assert_eq!(
            resolve_use("if cond:\n    mode = 'fast'\nprint(mode)\n", "mode"),
            local(BindingKind::Assignment)
        );
        assert_eq!(
            resolve_use("for i in xs:\n    seen = i\nprint(seen)\n", "seen"),
            local(BindingKind::Assignment)
        );
    }

    #[test]
    fn a_nested_function_does_not_leak_its_bindings() {
        assert_eq!(
            resolve_use(
                "def outer():\n    def inner():\n        cache = {}\n    return cache\n",
                "cache"
            ),
            None
        );
    }

    #[test]
    fn a_method_does_not_see_class_level_names() {
        // Python requires `self.LIMIT` or `C.LIMIT`; a bare `LIMIT` in a method is not the
        // class attribute. Treating the class body as an ordinary enclosing scope would
        // report a binding Python does not provide.
        assert_eq!(
            resolve_use(
                "class C:\n    LIMIT = 10\n    def m(self):\n        return LIMIT\n",
                "LIMIT"
            ),
            None
        );
    }

    #[test]
    fn a_class_body_sees_its_own_names() {
        assert_eq!(
            resolve_use(
                "class C:\n    LIMIT = 10\n    DOUBLE = LIMIT * 2\n",
                "LIMIT"
            ),
            local(BindingKind::Assignment)
        );
    }

    #[test]
    fn a_comprehension_target_is_scoped_to_the_comprehension() {
        assert_eq!(
            resolve_use("xs = [item for item in rows if item]\n", "item"),
            local(BindingKind::Comprehension)
        );
    }

    #[test]
    fn an_enclosing_function_binding_is_visible() {
        assert_eq!(
            resolve_use(
                "def outer():\n    conn = open()\n    def inner():\n        return conn\n",
                "conn"
            ),
            local(BindingKind::Assignment)
        );
    }

    // --- shadowing -----------------------------------------------------------------------

    #[test]
    fn a_local_shadows_an_import() {
        // The case that makes this worth having: a rule about `requests.get` must not fire
        // on a local `requests`.
        let source = "import requests\ndef f():\n    requests = fake()\n    return requests\n";
        assert_eq!(
            resolve_use(source, "requests"),
            local(BindingKind::Assignment)
        );
        assert!(shadowed(source, "requests"));
    }

    #[test]
    fn a_single_declaration_is_not_shadowing() {
        assert!(!shadowed(
            "import requests\nrequests.get(url)\n",
            "requests"
        ));
        assert!(!shadowed("def f():\n    x = 1\n    return x\n", "x"));
    }

    #[test]
    fn an_undeclared_name_resolves_to_nothing() {
        // A builtin, or a name from a star import. Either way this file does not declare it.
        assert_eq!(resolve_use("print(len(xs))\n", "len"), None);
    }

    #[test]
    fn a_non_identifier_resolves_to_nothing() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&Python.grammar())
            .expect("grammar loads");
        let source = "x = 1\n";
        let tree = parser.parse(source, None).expect("parses");
        let root = tree.root_node();
        assert_eq!(PythonBindingResolver.resolve(&tree, source, root), None);
        assert!(!PythonBindingResolver.is_shadowed(&tree, source, root));
    }
}
