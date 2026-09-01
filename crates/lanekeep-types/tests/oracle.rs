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

#[test]
fn each_predefined_type_annotation_is_its_primitive() {
    for (written, expected) in [
        ("number", Primitive::Number),
        ("string", Primitive::String),
        ("boolean", Primitive::Boolean),
        ("bigint", Primitive::BigInt),
        ("symbol", Primitive::Symbol),
    ] {
        assert_eq!(
            type_of_last(&format!("let x: {written};"), "type_annotation"),
            Some(Type::Primitive(expected)),
            "{written}"
        );
    }
}

/// `any` and `unknown` parse identically to `number`, and both must give nothing.
///
/// Asserted as *expected* rather than left to fall out. `any` is the absence of a claim,
/// and an oracle returning a type for it would assert something TypeScript does not.
#[test]
fn any_and_unknown_are_not_types_the_oracle_will_assert() {
    assert_eq!(type_of_last("let x: any;", "type_annotation"), None);
    assert_eq!(type_of_last("let x: unknown;", "type_annotation"), None);
}

#[test]
fn a_union_annotation_is_a_union_of_its_members() {
    let Some(Type::Union(members)) = type_of_last("let x: number | string;", "type_annotation")
    else {
        panic!("a two-member union");
    };
    assert_eq!(
        members,
        vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::String),
        ]
    );
}

/// The canonical ordering, asserted through the grammar rather than only through `union`.
#[test]
fn a_union_annotation_does_not_depend_on_the_order_written() {
    assert_eq!(
        type_of_last("let x: number | string;", "type_annotation"),
        type_of_last("let x: string | number;", "type_annotation")
    );
}

#[test]
fn a_literal_type_takes_its_literal_primitive() {
    assert_eq!(
        type_of_last("let x: 42;", "type_annotation"),
        Some(Type::Primitive(Primitive::Number))
    );
    assert_eq!(
        type_of_last("let x: 'a';", "type_annotation"),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn a_named_type_is_nominal() {
    assert_eq!(
        type_of_last(
            "import { Decimal } from 'decimal.js';\nlet x: Decimal;",
            "type_annotation"
        ),
        Some(Type::Nominal {
            name: "Decimal".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "Decimal".to_owned(),
                module: Some("decimal.js".to_owned()),
            }),
        })
    );
}

/// The shadow pair for nominals: a local class shares the name and not the module.
#[test]
fn a_locally_declared_type_is_nominal_with_no_module() {
    assert_eq!(
        type_of_last("class Decimal {}\nlet x: Decimal;", "type_annotation"),
        Some(Type::Nominal {
            name: "Decimal".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "Decimal".to_owned(),
                module: None,
            }),
        })
    );
}

#[test]
fn a_function_type_annotation_is_not_typed() {
    assert_eq!(
        type_of_last("let x: () => number;", "type_annotation"),
        None
    );
}

/// The pair for `bigint`'s text-matched shortcut, same shape as
/// `a_shadowed_builtin_is_not_typed_by_the_builtin_table` one level up in the vocabulary.
///
/// A file declaring its own `bigint` must resolve to that declaration, not to the
/// primitive. Matching on text alone, before the resolver has a say, would silently type
/// this as `Primitive::BigInt` instead of the class the annotation actually names.
#[test]
fn a_locally_declared_bigint_shadows_the_primitive() {
    assert_eq!(
        type_of_last("class bigint {}\nlet x: bigint;", "type_annotation"),
        Some(Type::Nominal {
            name: "bigint".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "bigint".to_owned(),
                module: None,
            }),
        })
    );
}

/// The same guard, reached through an alias rather than a class.
///
/// `type bigint = string` shadows the primitive exactly as `class bigint {}` does above —
/// the resolver sees a local declaration either way, so the text-matched shortcut still
/// defers. What differs is where deferring leads: a class has nothing further to read and
/// stops at nominal, but an alias is followed to what it names. The right answer here is
/// the alias's target, not the primitive the name happens to spell.
#[test]
fn a_locally_aliased_bigint_resolves_through_the_alias() {
    assert_eq!(
        type_of_last("type bigint = string;\nlet x: bigint;", "type_annotation"),
        Some(Type::Primitive(Primitive::String))
    );
}

/// Type the last `identifier` whose text is `name`.
fn type_of_use(source: &str, name: &str) -> Option<Type> {
    let tree = parse(source);
    let oracle =
        TypeScriptOracle::for_file(&TypeScript, &tree, source).expect("TypeScript is supported");

    let found = nodes(&tree)
        .into_iter()
        .rfind(|node| node.kind() == "identifier" && source.get(node.byte_range()) == Some(name));
    oracle.type_of(found.unwrap_or_else(|| panic!("no use of `{name}`")))
}

#[test]
fn an_annotated_parameter_has_its_annotated_type() {
    assert_eq!(
        type_of_use(
            "function credit(amount: number) { return amount; }",
            "amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

#[test]
fn an_annotated_optional_parameter_has_its_annotated_type() {
    assert_eq!(
        type_of_use(
            "function credit(amount?: number) { return amount; }",
            "amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// The headline: a local whose type comes from what it was initialized with.
#[test]
fn a_local_takes_the_type_of_its_initializer() {
    assert_eq!(
        type_of_use(
            "const amount = parseFloat(raw);\nconst y = amount;",
            "amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// The annotation wins over the initializer, and this is the half that denies a real bug.
///
/// An implementation reading the initializer first would answer `number` here. The
/// declared type is what the program means.
#[test]
fn an_annotation_beats_the_initializer_it_sits_beside() {
    assert_eq!(
        type_of_use(
            "const amount: string = parseFloat(raw);\nconst y = amount;",
            "amount"
        ),
        Some(Type::Primitive(Primitive::String))
    );
}

#[test]
fn a_local_annotated_with_a_named_type_is_nominal() {
    assert_eq!(
        type_of_use(
            "import { Decimal } from 'decimal.js';\nfunction f(x: Decimal) { return x; }",
            "x"
        ),
        Some(Type::Nominal {
            name: "Decimal".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "Decimal".to_owned(),
                module: Some("decimal.js".to_owned()),
            }),
        })
    );
}

/// An imported *value* has no type this milestone can read.
///
/// Its declaration is in another file, which this oracle does not open. Cross-file
/// resolution is a later milestone; answering anything here would be a guess.
#[test]
fn an_imported_value_has_no_type_yet() {
    assert_eq!(
        type_of_use("import { total } from './m';\nconst y = total;", "total"),
        None
    );
}

#[test]
fn an_undeclared_name_has_no_type() {
    assert_eq!(type_of_use("const y = missing;", "missing"), None);
}

/// An initializer chain terminates rather than running away.
#[test]
fn a_chain_of_initializers_terminates() {
    let source = "const a = b;\nconst b = a;\nconst c = a;\n";
    assert_eq!(type_of_use(source, "c"), None);
}

#[test]
fn a_same_file_type_alias_resolves_to_what_it_aliases() {
    assert_eq!(
        type_of_last("type Amount = number;\nlet x: Amount;", "type_annotation"),
        Some(Type::Primitive(Primitive::Number))
    );
}

#[test]
fn an_alias_chain_resolves_through_every_link() {
    assert_eq!(
        type_of_last(
            "type A = number;\ntype B = A;\ntype C = B;\nlet x: C;",
            "type_annotation"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// A cycle terminates instead of running away.
///
/// `type A = B; type B = A` is accepted by the parser and is meaningless. The bound is what
/// makes this return rather than recurse until the stack ends.
#[test]
fn an_alias_cycle_terminates_without_an_answer() {
    assert_eq!(
        type_of_last("type A = B;\ntype B = A;\nlet x: A;", "type_annotation"),
        None
    );
}

/// An alias to something the oracle cannot type is not itself an answer.
#[test]
fn an_alias_to_an_untyped_type_is_untyped() {
    assert_eq!(
        type_of_last("type A = () => void;\nlet x: A;", "type_annotation"),
        None
    );
}
