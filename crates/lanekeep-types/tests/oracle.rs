//! The oracle, against real parse trees.
//!
//! Integration rather than unit tests because the oracle's whole job is reading a grammar's
//! output, and a hand-built tree would be a second opinion about node shapes rather than a
//! test of the first.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the lint's grant covers `#[test]` functions and `#[cfg(test)]` modules, and a \
              helper in an integration-test crate is neither — see AGENTS.md"
)]

use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_types::{Primitive, Type, TypeScriptOracle};
use tree_sitter::{Node, Tree};

/// Parse `source` with the TypeScript grammar.
fn parse(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&TypeScript.grammar())
        .expect("the TypeScript grammar loads");
    parser.parse(source, None).expect("the source parses")
}

/// Every node in the tree, in source order.
///
/// A cursor walk rather than indexing: `child_count` is a `usize` and `child` takes a
/// `u32`, and the cast between them trips `clippy::cast_possible_truncation`, which this
/// workspace denies.
fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        out.push(node);
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// The last node of `kind` in the tree, in source order.
///
/// The *last* one, because a test writes the interesting expression after whatever sets it
/// up — and because resolving a use rather than a declaration is what a rule does.
fn last_of<'t>(tree: &'t Tree, kind: &str) -> Node<'t> {
    nodes(tree)
        .into_iter()
        .rfind(|node| node.kind() == kind)
        .unwrap_or_else(|| panic!("no `{kind}` node in the tree"))
}

/// Type the last node of `kind` in `source`.
fn type_of_last(source: &str, kind: &str) -> Option<Type> {
    let tree = parse(source);
    let oracle =
        TypeScriptOracle::for_file(&TypeScript, &tree, source).expect("TypeScript is supported");
    oracle.type_of(last_of(&tree, kind))
}

#[test]
fn a_number_literal_is_a_number() {
    assert_eq!(
        type_of_last("const x = 42;", "number"),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// The pair that matters: `1n` parses as `(number)` too.
///
/// There is no distinct bigint literal node, so a trailing `n` in the source text is the
/// only thing separating the two. An oracle dispatching on kind alone types every bigint as
/// a number, silently — which is precisely the confusion a bigint rule exists to catch.
#[test]
fn a_bigint_literal_is_a_bigint_despite_parsing_as_a_number() {
    assert_eq!(
        type_of_last("const x = 42n;", "number"),
        Some(Type::Primitive(Primitive::BigInt))
    );
}

#[test]
fn a_string_literal_is_a_string() {
    assert_eq!(
        type_of_last("const x = 'a';", "string"),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn a_template_string_is_a_string() {
    assert_eq!(
        type_of_last("const x = `a${b}`;", "template_string"),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn the_boolean_literals_are_booleans() {
    assert_eq!(
        type_of_last("const x = true;", "true"),
        Some(Type::Primitive(Primitive::Boolean))
    );
    assert_eq!(
        type_of_last("const x = false;", "false"),
        Some(Type::Primitive(Primitive::Boolean))
    );
}

#[test]
fn null_and_undefined_are_their_own_primitives() {
    assert_eq!(
        type_of_last("const x = null;", "null"),
        Some(Type::Primitive(Primitive::Null))
    );
    assert_eq!(
        type_of_last("const x = undefined;", "undefined"),
        Some(Type::Primitive(Primitive::Undefined))
    );
}

#[test]
fn a_parenthesized_expression_is_its_inner_expression() {
    assert_eq!(
        type_of_last("const x = (42);", "parenthesized_expression"),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// The oracle refuses a grammar whose vocabulary it does not share.
///
/// Handed a Python tree it would ask TypeScript questions and get confident nonsense, so
/// the constructor is where that is stopped rather than at whatever call site forgot.
#[test]
fn a_grammar_that_does_not_speak_typescript_gets_no_oracle() {
    let source = "x = 1\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lanekeep_lang_python::Python.grammar())
        .expect("the Python grammar loads");
    let tree = parser.parse(source, None).expect("the source parses");

    assert!(TypeScriptOracle::for_file(&lanekeep_lang_python::Python, &tree, source).is_none());
}

#[test]
fn arithmetic_on_numbers_is_a_number() {
    assert_eq!(
        type_of_last("const x = 1 * 2;", "binary_expression"),
        Some(Type::Primitive(Primitive::Number))
    );
}

#[test]
fn a_comparison_is_a_boolean() {
    assert_eq!(
        type_of_last("const x = 1 < 2;", "binary_expression"),
        Some(Type::Primitive(Primitive::Boolean))
    );
}

#[test]
fn concatenation_with_a_string_is_a_string() {
    assert_eq!(
        type_of_last("const x = 'a' + 1;", "binary_expression"),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn mixing_a_number_and_a_bigint_is_not_typed() {
    assert_eq!(type_of_last("const x = 1 + 1n;", "binary_expression"), None);
}

/// `??` is deliberately outside the table, asserted as expected rather than incidental.
#[test]
fn an_operator_outside_the_table_is_not_typed() {
    assert_eq!(type_of_last("const x = a ?? b;", "binary_expression"), None);
}

#[test]
fn typeof_is_a_string() {
    assert_eq!(
        type_of_last("const x = typeof y;", "unary_expression"),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn a_builtin_conversion_has_the_type_it_converts_to() {
    assert_eq!(
        type_of_last("const x = parseFloat(s);", "call_expression"),
        Some(Type::Primitive(Primitive::Number))
    );
    assert_eq!(
        type_of_last("const x = String(v);", "call_expression"),
        Some(Type::Primitive(Primitive::String))
    );
}

/// The other half of the pair, and the one that denies a real bug.
///
/// A file declaring its own `parseFloat` must not be typed by the builtin table. Matching
/// on the name alone would type this as a number, and the rule reading it would report
/// about a value that is a `Decimal`.
#[test]
fn a_shadowed_builtin_is_not_typed_by_the_builtin_table() {
    assert_eq!(
        type_of_last(
            "function parseFloat(s: string) { return s; }\nconst x = parseFloat('1');",
            "call_expression"
        ),
        None
    );
}

#[test]
fn a_call_to_an_ordinary_function_is_not_typed() {
    assert_eq!(
        type_of_last("const x = myHelper(1);", "call_expression"),
        None
    );
}
