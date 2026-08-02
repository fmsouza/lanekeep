//! Syntactic binding resolution for Go.
//!
//! Go's scoping is genuinely block-structured, which makes this the most conventional of the
//! three resolvers — and two details make it the least like the others.
//!
//! **Package-level declarations are order-independent.** A function may call one declared
//! two hundred lines below it, and a `var` may reference a `const` declared after it. So the
//! file scope is scanned whole rather than up to the use, and there is no notion here of a
//! name not yet being in scope.
//!
//! **Several statements are scopes without being blocks.** `if x := f(); x != nil` binds `x`
//! across the condition, the consequence *and* the else branch; `for i := range xs` binds
//! across the loop; `switch t := v.(type)` binds a differently-typed `t` in each case clause.
//! Treating only `block` as a scope would attribute all of these to the enclosing function.
//!
//! What is deliberately not modeled is *ordering within a block*. Go binds a short variable
//! declaration only for the statements after it, so `x := x` reads the outer `x` on the
//! right. Resolution here answers where a name comes from, not whether a use is legal, and a
//! program where that distinction changes the answer does not compile.

use lanekeep_lang::binding::{Binding, BindingKind, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

/// Resolves Go identifiers to the declaration that introduced them.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoBindingResolver;

/// Node kinds that introduce a scope.
///
/// The statement forms are here because each binds a name that outlives its own header but
/// not the enclosing function — see the module documentation.
const SCOPE_KINDS: &[&str] = &[
    "source_file",
    "function_declaration",
    "method_declaration",
    "func_literal",
    // A generic type declaration scopes its own type parameters: the `T` in
    // `type Box[T any] struct{ v T }` is visible in the struct body and nowhere else.
    "type_spec",
    "block",
    "if_statement",
    "for_statement",
    "expression_switch_statement",
    "type_switch_statement",
    "select_statement",
    "expression_case",
    "type_case",
    "default_case",
    "communication_case",
];

/// Identifier kinds that can refer to a binding.
///
/// Three rather than one, because Go spells the same reference differently by position: a
/// package is an `identifier` in `fmt.Println` and a `package_identifier` in `http.Client`,
/// and a type is a `type_identifier` wherever it appears.
const REFERENCE_KINDS: &[&str] = &["identifier", "type_identifier", "package_identifier"];

impl GoBindingResolver {
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
            "function_declaration" | "method_declaration" | "func_literal" => {
                signature_binds(scope, source, name)
            }

            // Its own type parameters only. The name it declares belongs to the file scope,
            // which reaches it through `declares` rather than here.
            "type_spec" => scope
                .child_by_field_name("type_parameters")
                .filter(|parameters| parameter_list_binds(*parameters, source, name))
                .map(|_| Binding::Local(BindingKind::TypeParam)),

            // A block holds its statements one level down, in a `statement_list`.
            "block" => scope
                .named_children(&mut scope.walk())
                .find_map(|child| statements_bind(child, source, name)),

            // The header binds across the whole statement, including the else branch.
            "if_statement" | "expression_switch_statement" => scope
                .child_by_field_name("initializer")
                .and_then(|init| declares(init, source, name)),

            // Either `for i := 0; ...` or `for i := range xs`.
            "for_statement" => for_binds(scope, source, name),

            // `switch t := v.(type)` binds `t` afresh in every case clause.
            "type_switch_statement" => scope
                .child_by_field_name("alias")
                .filter(|alias| expression_list_binds(*alias, source, name))
                .map(|_| Binding::Local(BindingKind::Var)),

            // `source_file` and the case clauses, which scan the same way for different
            // reasons: the file block holds imports and every package-level declaration, in
            // any order, and a case clause holds its own statements directly rather than
            // wrapped in a block.
            _ => statements_bind(scope, source, name),
        }
    }
}

impl BindingResolver for GoBindingResolver {
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

        // More than one enclosing scope declares it, and the innermost is the one `resolve`
        // returns. One declaration, however nested, is not shadowing.
        Self::scopes(node)
            .into_iter()
            .filter(|scope| Self::declaration_in(*scope, source, name).is_some())
            .count()
            > 1
    }
}

/// Scan a node's direct children for a declaration of `name`.
///
/// Direct children only: a nested block, function literal or case clause is a scope of its
/// own, and reaching into one would bind names that are not visible here.
fn statements_bind(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    node.named_children(&mut node.walk())
        .find_map(|child| declares(child, source, name))
}

/// Whether this node is a declaration of `name`, and of what kind.
fn declares(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    match node.kind() {
        "import_declaration" => import_binds(node, source, name),

        // `var`, `const` and `type` each wrap either one spec or a parenthesized list, and a
        // list wraps its specs again — so both layers recurse the same way.
        "var_declaration" | "const_declaration" | "type_declaration" | "var_spec_list"
        | "const_spec_list" => node
            .named_children(&mut node.walk())
            .find_map(|child| declares(child, source, name)),

        "var_spec" => {
            named_field_binds(node, source, name).then_some(Binding::Local(BindingKind::Var))
        }
        "const_spec" => {
            named_field_binds(node, source, name).then_some(Binding::Local(BindingKind::Const))
        }
        "type_spec" | "type_alias" => {
            named_field_binds(node, source, name).then_some(Binding::Local(BindingKind::Type))
        }

        // `x := 1`. A variable declaration, so `var` rather than `assignment` — the spelling
        // differs from `var x = 1` but what it introduces does not, and a rule that cares
        // about the spelling can ask the node.
        "short_var_declaration" => node
            .child_by_field_name("left")
            .filter(|left| expression_list_binds(*left, source, name))
            .map(|_| Binding::Local(BindingKind::Var)),

        "function_declaration" | "method_declaration" => node
            .child_by_field_name("name")
            .filter(|declared| node_text(*declared, source) == name)
            .map(|_| Binding::Local(BindingKind::Function)),

        // A labeled statement binds its label in the function, in a namespace of its own —
        // `break outer` cannot collide with a variable called `outer`. Reporting it as a
        // binding would let a rule confuse the two.
        _ => None,
    }
}

/// Whether a declaration's `name` field — which repeats, for `var a, b int` — includes `name`.
fn named_field_binds(node: Node<'_>, source: &str, name: &str) -> bool {
    node.children_by_field_name("name", &mut node.walk())
        .any(|declared| node_text(declared, source) == name)
}

/// Whether an `expression_list` of targets includes `name`.
fn expression_list_binds(list: Node<'_>, source: &str, name: &str) -> bool {
    // `_` is the blank identifier: it discards rather than binds, so a use of `_` elsewhere
    // must not resolve to it.
    name != "_"
        && list
            .named_children(&mut list.walk())
            .any(|target| target.kind() == "identifier" && node_text(target, source) == name)
}

/// A `for` statement's own bindings: the init clause, or the range targets.
fn for_binds(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    // `for i := 0; i < 3; i++` wraps its three clauses in a `for_clause`, so the initializer
    // is a grandchild rather than a field of the statement. Reading the field off the
    // statement finds nothing and silently attributes `i` to the enclosing function.
    if let Some(found) = scope
        .named_children(&mut scope.walk())
        .filter(|child| child.kind() == "for_clause")
        .find_map(|clause| {
            clause
                .child_by_field_name("initializer")
                .and_then(|init| declares(init, source, name))
        })
    {
        return Some(found);
    }

    // `for i, v := range xs`. The clause is an unnamed child rather than a field.
    scope
        .named_children(&mut scope.walk())
        .filter(|child| child.kind() == "range_clause")
        .find_map(|clause| {
            clause
                .child_by_field_name("left")
                .filter(|left| expression_list_binds(*left, source, name))
                .map(|_| Binding::Local(BindingKind::Loop))
        })
}

/// A function's receiver, type parameters, parameters and named results.
fn signature_binds(scope: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    if let Some(receiver) = scope.child_by_field_name("receiver")
        && parameter_list_binds(receiver, source, name)
    {
        return Some(Binding::Local(BindingKind::Receiver));
    }

    if let Some(parameters) = scope.child_by_field_name("type_parameters")
        && parameter_list_binds(parameters, source, name)
    {
        return Some(Binding::Local(BindingKind::TypeParam));
    }

    if let Some(parameters) = scope.child_by_field_name("parameters")
        && parameter_list_binds(parameters, source, name)
    {
        return Some(Binding::Local(BindingKind::Param));
    }

    // Named results — the `err` in `func f() (err error)` — are bound like parameters and
    // are assignable in the body, which is the whole point of naming them. When the result
    // is a bare type rather than a list, there is nothing named to find.
    scope
        .child_by_field_name("result")
        .filter(|result| result.kind() == "parameter_list")
        .filter(|result| parameter_list_binds(*result, source, name))
        .map(|_| Binding::Local(BindingKind::Param))
}

/// Whether a parameter list declares `name`.
fn parameter_list_binds(list: Node<'_>, source: &str, name: &str) -> bool {
    list.named_children(&mut list.walk()).any(|declaration| {
        matches!(
            declaration.kind(),
            "parameter_declaration"
                | "variadic_parameter_declaration"
                | "type_parameter_declaration"
        ) && named_field_binds(declaration, source, name)
    })
}

/// Whether an import declaration binds `name`, and to which package.
fn import_binds(node: Node<'_>, source: &str, name: &str) -> Option<Binding> {
    specs(node).find_map(|spec| {
        let path = spec.child_by_field_name("path")?;
        let module = string_content(path, source);

        match spec.child_by_field_name("name") {
            // `import f "strings"` — the alias is the only name it binds.
            Some(alias) if alias.kind() == "package_identifier" => {
                (node_text(alias, source) == name).then(|| import_of(&module))
            }
            // `import _ "embed"` binds nothing; the import is for its side effects.
            // `import . "math"` binds every exported name of the package, which cannot be
            // known without reading it — so nothing is claimed rather than guessed.
            Some(_) => None,
            // `import "net/http"` binds `http`: the last segment, not the whole path.
            None => (last_segment(&module) == name).then(|| import_of(&module)),
        }
    })
}

/// The specs of an import declaration, whether parenthesized or not.
fn specs(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "import_spec" => out.push(child),
            "import_spec_list" => {
                let mut inner = child.walk();
                out.extend(
                    child
                        .named_children(&mut inner)
                        .filter(|spec| spec.kind() == "import_spec"),
                );
            }
            _ => {}
        }
    }
    out.into_iter()
}

/// A Go import binds the package as a whole, which is what `Namespace` means.
fn import_of(module: &str) -> Binding {
    Binding::Import {
        module: module.to_owned(),
        name: ImportedName::Namespace,
    }
}

/// The last path segment — `http` of `net/http`.
fn last_segment(module: &str) -> &str {
    module.rsplit('/').next().unwrap_or(module)
}

/// The text inside a string literal, without its quotes.
fn string_content(literal: Node<'_>, source: &str) -> String {
    literal
        .named_children(&mut literal.walk())
        .find(|child| child.kind().ends_with("_content"))
        .map_or_else(
            || {
                node_text(literal, source)
                    .trim_matches(['"', '`'])
                    .to_owned()
            },
            |content| node_text(content, source).to_owned(),
        )
}

fn node_text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;

    use super::*;
    use crate::Go;

    /// Resolve the last identifier in the source that reads exactly `name`.
    ///
    /// Rules resolve a *use*, so tests must too — resolving the declaration site exercises a
    /// different path than the one that matters.
    fn resolve_use(source: &str, name: &str) -> Option<Binding> {
        with_use(source, name, |tree, node| {
            GoBindingResolver.resolve(tree, source, node)
        })
    }

    fn shadowed(source: &str, name: &str) -> bool {
        with_use(source, name, |tree, node| {
            GoBindingResolver.is_shadowed(tree, source, node)
        })
    }

    fn with_use<T>(source: &str, name: &str, f: impl Fn(&Tree, Node<'_>) -> T) -> T {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&Go.grammar()).expect("grammar loads");
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

    fn whole(module: &str) -> Binding {
        Binding::Import {
            module: module.to_owned(),
            name: ImportedName::Namespace,
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

    /// A file with `body` at package level.
    fn pkg(body: &str) -> String {
        format!("package main\n\n{body}")
    }

    // --- imports -----------------------------------------------------------------------

    #[test]
    fn a_plain_import_binds_the_package_name() {
        assert_eq!(
            resolve_use(
                &pkg("import \"fmt\"\n\nfunc f() { fmt.Println() }\n"),
                "fmt"
            ),
            Some(whole("fmt"))
        );
    }

    #[test]
    fn a_dotted_path_binds_only_its_last_segment() {
        // `import "net/http"` binds `http` while carrying the whole path as the module. A
        // rule matching on the module needs `net/http`; a rule matching the identifier sees
        // only `http`, and conflating the two makes one of them always wrong.
        //
        // There is no assertion about `net` here because there is no `net` node to resolve —
        // it exists only inside the string literal.
        let source = pkg("import \"net/http\"\n\nfunc f() { http.Get(\"\") }\n");
        assert_eq!(resolve_use(&source, "http"), Some(whole("net/http")));
    }

    #[test]
    fn an_aliased_import_resolves_under_its_alias() {
        assert_eq!(
            resolve_use(
                &pkg("import f \"strings\"\n\nfunc g() { f.Split(\"\", \"\") }\n"),
                "f"
            ),
            Some(whole("strings"))
        );
    }

    #[test]
    fn an_aliased_import_does_not_also_bind_the_original_name() {
        // The point of an alias is that the original name is *not* in scope.
        assert_eq!(
            resolve_use(
                &pkg("import f \"strings\"\n\nfunc g() { strings.Split(\"\", \"\") }\n"),
                "strings"
            ),
            None
        );
    }

    #[test]
    fn a_blank_import_binds_nothing() {
        // `import _ "embed"` is for its side effects. Reporting `_` as bound to it would
        // make every discarded value resolve to a package.
        assert_eq!(
            resolve_use(&pkg("import _ \"embed\"\n\nfunc f() { _ = 1 }\n"), "_"),
            None
        );
    }

    #[test]
    fn a_dot_import_claims_nothing() {
        // A dot import brings every exported name into file scope. Which names those are
        // cannot be known without reading the package, so nothing is claimed rather than
        // guessed at.
        assert_eq!(
            resolve_use(&pkg("import . \"math\"\n\nfunc f() { _ = Pi }\n"), "Pi"),
            None
        );
    }

    #[test]
    fn a_parenthesized_import_group_binds_every_spec() {
        let source = pkg(
            "import (\n\t\"fmt\"\n\ts \"strings\"\n)\n\nfunc f() { fmt.Println(s.Title(\"\")) }\n",
        );
        assert_eq!(resolve_use(&source, "fmt"), Some(whole("fmt")));
        assert_eq!(resolve_use(&source, "s"), Some(whole("strings")));
    }

    // --- package-level declarations ------------------------------------------------------

    #[test]
    fn a_package_level_var_is_visible_in_a_function() {
        assert_eq!(
            resolve_use(
                &pkg("var client int\n\nfunc f() { _ = client }\n"),
                "client"
            ),
            local(BindingKind::Var)
        );
    }

    #[test]
    fn a_package_level_declaration_is_visible_before_it_is_written() {
        // Go's package block is order-independent, which is the property that makes this
        // resolver different from the Python one rather than a copy of it.
        assert_eq!(
            resolve_use(&pkg("func f() { _ = Max }\n\nconst Max = 3\n"), "Max"),
            local(BindingKind::Const)
        );
    }

    #[test]
    fn a_grouped_var_declaration_binds_every_spec() {
        let source = pkg("var (\n\ta int\n\tb string\n)\n\nfunc f() { _, _ = a, b }\n");
        assert_eq!(resolve_use(&source, "a"), local(BindingKind::Var));
        assert_eq!(resolve_use(&source, "b"), local(BindingKind::Var));
    }

    #[test]
    fn a_multi_name_var_spec_binds_all_of_its_names() {
        // `name` is a repeated field. Reading only the first would silently lose `b`.
        let source = pkg("var a, b = 1, 2\n\nfunc f() { _, _ = a, b }\n");
        assert_eq!(resolve_use(&source, "a"), local(BindingKind::Var));
        assert_eq!(resolve_use(&source, "b"), local(BindingKind::Var));
    }

    #[test]
    fn a_struct_type_is_a_type_rather_than_a_class() {
        assert_eq!(
            resolve_use(&pkg("type Repo struct{}\n\nfunc f(r Repo) {}\n"), "Repo"),
            local(BindingKind::Type)
        );
    }

    #[test]
    fn an_interface_and_an_alias_are_types_too() {
        assert_eq!(
            resolve_use(&pkg("type S interface{}\n\nfunc f(s S) {}\n"), "S"),
            local(BindingKind::Type)
        );
        assert_eq!(
            resolve_use(&pkg("type A = int\n\nfunc f(a A) {}\n"), "A"),
            local(BindingKind::Type)
        );
    }

    #[test]
    fn a_function_declaration_binds_its_name() {
        assert_eq!(
            resolve_use(
                &pkg("func helper() {}\n\nfunc f() { helper() }\n"),
                "helper"
            ),
            local(BindingKind::Function)
        );
    }

    // --- signatures ------------------------------------------------------------------------

    #[test]
    fn a_receiver_is_a_receiver_rather_than_a_parameter() {
        // The distinction rules ask about: "must the receiver be a pointer", "is the
        // receiver name consistent across methods".
        assert_eq!(
            resolve_use(&pkg("type R struct{}\n\nfunc (r *R) M() { _ = r }\n"), "r"),
            local(BindingKind::Receiver)
        );
    }

    #[test]
    fn parameters_bind_including_the_variadic_and_the_shared_type_form() {
        let source = pkg("func f(x, y int, rest ...string) { _, _, _ = x, y, rest }\n");
        assert_eq!(resolve_use(&source, "x"), local(BindingKind::Param));
        assert_eq!(resolve_use(&source, "y"), local(BindingKind::Param));
        assert_eq!(resolve_use(&source, "rest"), local(BindingKind::Param));
    }

    #[test]
    fn a_named_result_binds_like_a_parameter() {
        // The reason to name a result is to assign to it, so it has to resolve.
        assert_eq!(
            resolve_use(
                &pkg("func f() (err error) {\n\terr = nil\n\treturn\n}\n"),
                "err"
            ),
            local(BindingKind::Param)
        );
    }

    #[test]
    fn a_type_parameter_binds_on_a_function_and_on_a_type() {
        assert_eq!(
            resolve_use(&pkg("func F[T any](v T) T { return v }\n"), "T"),
            local(BindingKind::TypeParam)
        );
        assert_eq!(
            resolve_use(&pkg("type Box[T any] struct{ v T }\n"), "T"),
            local(BindingKind::TypeParam)
        );
    }

    // --- block-structured scoping ---------------------------------------------------------

    #[test]
    fn a_short_variable_declaration_binds_in_its_block() {
        assert_eq!(
            resolve_use(&pkg("func f() {\n\tx := 1\n\t_ = x\n}\n"), "x"),
            local(BindingKind::Var)
        );
    }

    #[test]
    fn a_range_clause_binds_its_targets_as_loop_variables() {
        let source = pkg("func f(xs []int) {\n\tfor i, v := range xs {\n\t\t_, _ = i, v\n\t}\n}\n");
        assert_eq!(resolve_use(&source, "i"), local(BindingKind::Loop));
        assert_eq!(resolve_use(&source, "v"), local(BindingKind::Loop));
    }

    #[test]
    fn a_three_clause_for_binds_its_initializer() {
        assert_eq!(
            resolve_use(
                &pkg("func f() {\n\tfor i := 0; i < 3; i++ {\n\t\t_ = i\n\t}\n}\n"),
                "i"
            ),
            local(BindingKind::Var)
        );
    }

    #[test]
    fn an_if_initializer_is_visible_in_the_else_branch() {
        // Go scopes it across the whole statement. Binding it to the consequence alone would
        // report the else branch's use as unresolved.
        let source = pkg("func f() {\n\tif v := g(); v != nil {\n\t} else {\n\t\t_ = v\n\t}\n}\n");
        assert_eq!(resolve_use(&source, "v"), local(BindingKind::Var));
    }

    #[test]
    fn a_type_switch_alias_binds_inside_each_case() {
        let source =
            pkg("func f(v any) {\n\tswitch t := v.(type) {\n\tcase int:\n\t\t_ = t\n\t}\n}\n");
        assert_eq!(resolve_use(&source, "t"), local(BindingKind::Var));
    }

    #[test]
    fn a_function_literal_has_its_own_parameters() {
        assert_eq!(
            resolve_use(
                &pkg("func f() {\n\tg := func(inner int) { _ = inner }\n\t_ = g\n}\n"),
                "inner"
            ),
            local(BindingKind::Param)
        );
    }

    #[test]
    fn a_declaration_in_a_nested_block_does_not_leak_outwards() {
        // The property that makes block scoping worth modeling at all.
        let source = pkg(
            "func f() {\n\t{\n\t\tinner := 1\n\t\t_ = inner\n\t}\n}\n\nfunc g() { _ = inner }\n",
        );
        assert_eq!(resolve_use(&source, "inner"), None);
    }

    #[test]
    fn the_blank_identifier_never_resolves() {
        // `_` discards. If it bound, every `_ = x` in a file would make `_` resolve to a
        // variable and rules counting assignments would see phantom ones.
        assert_eq!(
            resolve_use(&pkg("func f() {\n\t_ = 1\n\t_ = 2\n}\n"), "_"),
            None
        );
    }

    #[test]
    fn an_undeclared_name_resolves_to_nothing() {
        assert_eq!(
            resolve_use(&pkg("func f() { _ = missing }\n"), "missing"),
            None
        );
    }

    // --- shadowing ---------------------------------------------------------------------------

    #[test]
    fn a_block_local_shadowing_a_package_level_name_is_shadowed() {
        let source = pkg("var x = 1\n\nfunc f() {\n\tx := 2\n\t_ = x\n}\n");
        assert!(shadowed(&source, "x"));
        assert_eq!(resolve_use(&source, "x"), local(BindingKind::Var));
    }

    #[test]
    fn a_single_declaration_however_nested_is_not_shadowing() {
        assert!(!shadowed(&pkg("func f() {\n\tx := 1\n\t_ = x\n}\n"), "x"));
    }

    #[test]
    fn a_parameter_shadowing_an_import_is_shadowed() {
        // The case that makes import-based rules wrong when it is missed: a parameter named
        // `url` has nothing to do with the `net/url` package.
        let source = pkg("import \"net/url\"\n\nfunc f(url string) { _ = url }\n");
        assert!(shadowed(&source, "url"));
        assert_eq!(resolve_use(&source, "url"), local(BindingKind::Param));
    }

    #[test]
    fn a_non_identifier_node_resolves_to_nothing() {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&Go.grammar()).expect("grammar loads");
        let source = pkg("func f() { _ = 1 }\n");
        let tree = parser.parse(&source, None).expect("parses");
        let root = tree.root_node();
        assert_eq!(GoBindingResolver.resolve(&tree, &source, root), None);
        assert!(!GoBindingResolver.is_shadowed(&tree, &source, root));
    }
}
