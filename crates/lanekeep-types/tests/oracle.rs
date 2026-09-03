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
use lanekeep_types::{Primitive, Type, TypeScriptOracle, TypeScriptSupport};
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
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let oracle = TypeScriptOracle::new(&support, &tree, source);
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

#[test]
fn a_grammar_that_speaks_typescript_yields_support() {
    assert!(TypeScriptSupport::probe(&TypeScript).is_some());
}

/// The guard PR 1 established, now living on the probe.
#[test]
fn a_grammar_that_does_not_speak_typescript_yields_no_support() {
    assert!(TypeScriptSupport::probe(&lanekeep_lang_python::Python).is_none());
}

/// One token serves many files, which is the entire point of the split.
#[test]
fn one_probe_serves_many_files() {
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    for (source, kind, expected) in [
        ("const a = 1;", "number", Type::Primitive(Primitive::Number)),
        (
            "const b = 'x';",
            "string",
            Type::Primitive(Primitive::String),
        ),
        (
            "const c = 1n;",
            "number",
            Type::Primitive(Primitive::BigInt),
        ),
    ] {
        let tree = parse(source);
        let oracle = TypeScriptOracle::new(&support, &tree, source);
        assert_eq!(
            oracle.type_of(last_of(&tree, kind)),
            Some(expected),
            "{source}"
        );
    }
}

#[test]
fn arithmetic_on_numbers_is_a_number() {
    assert_eq!(
        type_of_last("const x = 1 * 2;", "binary_expression"),
        Some(Type::Primitive(Primitive::Number))
    );
}

#[test]
fn arithmetic_on_bigints_is_a_bigint() {
    assert_eq!(
        type_of_last("const x = 2n * 3n;", "binary_expression"),
        Some(Type::Primitive(Primitive::BigInt))
    );
}

/// The half that denies the bug: `*` is not a number just because it is `*`.
///
/// This arm used to fall through to `number` whenever the pair was not two bigints, which
/// meant an operand nothing had established still produced a confident primitive. The
/// first case below is a bigint answered as a number — the very confusion a bigint rule
/// exists to catch — and the second is a `TypeError` answered as a value.
#[test]
fn arithmetic_with_an_operand_the_oracle_cannot_type_is_not_a_number() {
    assert_eq!(
        type_of_last(
            "import { total } from './m';\nconst z = total * 2n;",
            "binary_expression"
        ),
        None
    );
    assert_eq!(
        type_of_last(
            "class D {}\nconst z = new D() * new D();",
            "binary_expression"
        ),
        None
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

/// The union's own denying half: a member that cannot be typed sinks the whole union.
///
/// Dropping it and keeping the rest returns a bare `Primitive(Number)` here — identical in
/// every byte to a declared `number`, with nothing left to say a member was lost. A rule
/// reporting "this is typed `number`" then fires on `amount: number | Decimal` and accuses
/// correct code, which is the failure the whole oracle is arranged against.
#[test]
fn a_union_with_a_member_the_oracle_cannot_type_is_not_typed_at_all() {
    assert_eq!(
        type_of_last("let x: number | Foo<T>;", "type_annotation"),
        None
    );
    assert_eq!(
        type_of_last("let x: number | (string | boolean);", "type_annotation"),
        None
    );
    assert_eq!(
        type_of_last("let x: number[] | string;", "type_annotation"),
        None
    );
}

/// And the guard against over-refusing it: a comment is not a member.
///
/// A `comment` is a *named* child of a `union_type`, so an all-or-nothing walk that only
/// filtered on `is_named` would see one as a member it could not type — and a comment
/// written inside an annotation would silence the union. The union still has to survive
/// being commented.
#[test]
fn a_comment_inside_a_union_is_not_a_member_of_it() {
    assert_eq!(
        type_of_last("let x: number /* which one */ | string;", "type_annotation"),
        type_of_last("let x: number | string;", "type_annotation")
    );
    assert!(matches!(
        type_of_last("let x: number /* which one */ | string;", "type_annotation"),
        Some(Type::Union(_))
    ));
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

/// A renamed import's `Symbol.name` is the local alias, not the exported name.
///
/// `Symbol::name` is filled from the reference's own text, and the resolver's
/// `ImportedName::Named` — which does carry `Decimal` — is never consulted for it. Nothing
/// above catches this: `a_named_type_is_nominal` imports `Decimal` under its own name, so
/// the use site and the export happen to read identically and the two provenances are
/// indistinguishable from that test alone.
#[test]
fn a_renamed_import_s_symbol_name_is_the_local_alias() {
    assert_eq!(
        type_of_last(
            "import { Decimal as Money } from 'decimal.js';\nlet x: Money;",
            "type_annotation"
        ),
        Some(Type::Nominal {
            name: "Money".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "Money".to_owned(),
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

/// The third case, and the one nothing else here covers: a nominal type with **no** symbol.
///
/// `Date` is an ambient global — declared in a lib the oracle does not open, imported from
/// nowhere, shadowed by nothing — so the resolver has nothing to say and the symbol is absent.
/// Ordinary rather than a corner case: every global and every ambient declaration lands here.
///
/// The two tests above cannot stand in for it. Both assert `symbol: Some(..)`, so an oracle
/// that fabricated a symbol from the type's own name whenever the resolver came back empty
/// would leave them green — and a rule branching on `symbol` would then read an unattributed
/// global as a resolved local declaration, which is the wrong answer rather than no answer.
#[test]
fn an_ambient_type_is_nominal_with_no_symbol() {
    assert_eq!(
        type_of_last("let x: Date;", "type_annotation"),
        Some(Type::Nominal {
            name: "Date".to_owned(),
            symbol: None,
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
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let oracle = TypeScriptOracle::new(&support, &tree, source);

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

/// A parameter bound through a pattern is not given the pattern's own type.
///
/// `Money` is the type of the object being taken apart, not of `rate` taken out of it, and
/// the resolver hands back the same `required_parameter` for every name the pattern binds.
/// Reading the annotation regardless answers `Money` for `rate` — a confident type for a
/// name whose real one needs a property lookup this milestone does not have.
#[test]
fn a_destructured_parameter_is_not_given_its_pattern_s_type() {
    for source in [
        "function credit({ rate }: Money) { return rate; }",
        "function credit({ rate }?: Money) { return rate; }",
        "function credit([rate]: Money) { return rate; }",
        "const credit = ({ rate }: Money) => rate;",
    ] {
        assert_eq!(type_of_use(source, "rate"), None, "{source}");
    }
}

/// The other half: a plain parameter still answers, so the guard did not silence everything.
///
/// `let a!: number` is the adjacent shape worth pinning — measured, a definite-assignment
/// `!` leaves the bound name an `identifier`, so it is not mistaken for a pattern.
#[test]
fn the_pattern_guard_leaves_a_plain_binding_alone() {
    assert_eq!(
        type_of_use(
            "function credit(amount: number) { return amount; }",
            "amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
    assert_eq!(
        type_of_use("let amount!: number;\nconst y = amount;", "amount"),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// The same guard on a variable declarator, where both paths were wrong.
///
/// The initializer's type belongs to what was destructured, not to any name taken out of
/// it: `String(q)` is a string and `s.length` is a number. The annotation is wrong the same
/// way — `Money` is the type of `order`, and `rate` is a property of it.
#[test]
fn a_destructured_local_is_not_given_its_initializer_or_annotation_type() {
    assert_eq!(
        type_of_use(
            "const s = String(q);\nconst { length } = s;\nconst y = length;",
            "length"
        ),
        None
    );
    assert_eq!(
        type_of_use("const { rate }: Money = order;\nconst y = rate;", "rate"),
        None
    );
    assert_eq!(
        type_of_use("const [first] = xs;\nconst y = first;", "first"),
        None
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

/// A generic type parameter is not answered by an outer alias that shares its name.
///
/// The scope walk had no idea a signature declared `A`, so it escaped outward and found
/// `type A = number` — and `type_of(x)` came back `number` for a value that is whatever
/// the call site chose. That is a confidently wrong type where the honest answer is
/// nothing, on every shape that can carry a type parameter.
#[test]
fn a_type_parameter_is_not_answered_by_an_outer_alias_of_the_same_name() {
    for source in [
        "type A = number;\nfunction f<A>(x: A) { return x; }",
        "type A = number;\nclass C<A> { m(x: A) { return x; } }",
        "type A = number;\nclass C { m<A>(x: A) { return x; } }",
        "type A = number;\nconst f = <A,>(x: A) => x;",
    ] {
        assert_eq!(type_of_use(source, "x"), None, "{source}");
    }
}

/// The other half: without a type parameter in the way, the alias is still followed.
///
/// The fix must not have made every annotated parameter unknowable — it is the *shadow*
/// that is new, and one identical file minus the `<A>` has to keep answering `number`.
#[test]
fn an_alias_still_answers_a_parameter_that_no_type_parameter_shadows() {
    assert_eq!(
        type_of_use("type A = number;\nfunction f(x: A) { return x; }", "x"),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// A `for...of` loop variable is not answered by an outer binding of the same name.
///
/// `for_in_statement` was in the resolver's scope list and bound nothing, so the walk went
/// straight past the loop head to whatever was outside it — and `type_of(x)` inside the
/// body came back `string` from a `const x = 'a'` the loop shadows entirely.
#[test]
fn a_for_of_loop_variable_is_not_answered_by_the_binding_it_shadows() {
    for source in [
        "const x = 'a';\nfor (const x of ns) { g(x); }",
        "const x = 'a';\nfor (let x of ns) { g(x); }",
        "const x = 'a';\nfor (const x in ns) { g(x); }",
    ] {
        assert_eq!(type_of_use(source, "x"), None, "{source}");
    }
}

/// The two halves that deny an over-eager version of the fix.
///
/// A head with no `const` / `let` / `var` declares nothing — `for (x of ns)` assigns to a
/// name that already exists — so binding it would invent a shadow the program does not
/// have. And a loop that binds some *other* name must leave the outer one reachable.
#[test]
fn a_loop_head_that_declares_nothing_leaves_the_outer_binding_reachable() {
    assert_eq!(
        type_of_use("const x = 'a';\nfor (x of ns) { g(x); }", "x"),
        Some(Type::Primitive(Primitive::String))
    );
    assert_eq!(
        type_of_use("const x = 'a';\nfor (const y of ns) { g(x); }", "x"),
        Some(Type::Primitive(Primitive::String))
    );
}

/// The `Debug` impl identifies the file rather than reproducing it.
///
/// `source` is a whole file. Printing it puts that file into every log line the oracle
/// appears in, which is what `LanguageRegistry` prints keys rather than languages to avoid.
#[test]
fn the_debug_impl_does_not_print_the_whole_source() {
    let source = "const aNameThatMustNotReachALogLine = 1;";
    let tree = parse(source);
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let oracle = TypeScriptOracle::new(&support, &tree, source);

    let rendered = format!("{oracle:?}");
    assert!(
        !rendered.contains("aNameThatMustNotReachALogLine"),
        "{rendered}"
    );
    assert!(rendered.contains("source_len"), "{rendered}");
}

/// The symbol of the last use of `name`.
fn symbol_of_use(source: &str, name: &str) -> Option<lanekeep_types::Symbol> {
    let tree = parse(source);
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let oracle = TypeScriptOracle::new(&support, &tree, source);

    let found = nodes(&tree).into_iter().rfind(|node| {
        matches!(node.kind(), "identifier" | "type_identifier")
            && source.get(node.byte_range()) == Some(name)
    });
    oracle.symbol_of(found.unwrap_or_else(|| panic!("no use of `{name}`")))
}

#[test]
fn an_imported_name_carries_the_module_it_came_from() {
    assert_eq!(
        symbol_of_use(
            "import { Decimal } from 'decimal.js';\nconst x = Decimal;",
            "Decimal"
        ),
        Some(lanekeep_types::Symbol {
            name: "Decimal".to_owned(),
            module: Some("decimal.js".to_owned()),
        })
    );
}

/// The shadow pair: the same name, locally declared, carries no module.
#[test]
fn a_locally_declared_name_carries_no_module() {
    assert_eq!(
        symbol_of_use("class Decimal {}\nconst x = Decimal;", "Decimal"),
        Some(lanekeep_types::Symbol {
            name: "Decimal".to_owned(),
            module: None,
        })
    );
}

#[test]
fn a_name_nothing_declares_has_no_symbol() {
    assert_eq!(symbol_of_use("const x = missing;", "missing"), None);
}

/// Two runs over one input agree, byte for byte.
///
/// The ordering guarantee's own test shape. Nothing here reads a clock or iterates a hash
/// map, and this is what would notice if that stopped being true.
#[test]
fn two_runs_over_one_input_agree() {
    let source = "import { Decimal } from 'decimal.js';\n\
                  type Amount = number | string;\n\
                  function f(a: Amount, b: Decimal) { const c = parseFloat('1'); return c; }\n";
    let first = format!("{:?}", type_of_use(source, "c"));
    let second = format!("{:?}", type_of_use(source, "c"));
    assert_eq!(first, second);

    let one = format!("{:?}", type_of_last(source, "union_type"));
    let other = format!("{:?}", type_of_last(source, "union_type"));
    assert_eq!(one, other);
}

// --- type parameters on declaration kinds that were not scopes ------------------------
//
// The oracle's half of the resolver fix in `lanekeep-lang-js`. These four kinds carry a
// `type_parameters` field and were not in `SCOPE_KINDS`, so the walk escaped outward and an
// outer alias of the same name answered instead. The result is worse than a missing answer:
// it is a *confident* one, identical in every byte to a declared `number`, with nothing
// anywhere to say a type parameter was passed over.

/// A type parameter is whatever the call site chose, so the oracle says nothing about it.
#[test]
fn a_type_parameter_on_any_declaration_kind_gives_nothing() {
    for source in [
        "interface O<T> { x: T }",
        "type O<T> = { x: T };",
        "abstract class C<T> { abstract x: T }",
        "declare function f<T>(x: T): T;",
    ] {
        assert_eq!(type_of_last(source, "type_annotation"), None, "{source}");
    }
}

/// And it shadows an outer alias, which is the case that used to answer wrongly.
///
/// Distinct from the test above rather than a restatement of it. Without an alias in scope
/// the old behavior produced `Nominal { name: "T", symbol: None }`, which a rule checking a
/// `require` reports; with one it produced `Some(Primitive(Number))`, which a rule checking
/// `forbid` reports. Two different wrong answers, and only the second is visible here.
#[test]
fn a_type_parameter_shadowing_an_alias_does_not_answer_with_the_alias() {
    for source in [
        "type A = number;\ninterface O<A> { x: A }",
        "type A = number;\ntype O<A> = { x: A };",
        "type A = number;\nabstract class C<A> { abstract x: A }",
        "type A = number;\ndeclare function f<A>(x: A): A;",
    ] {
        assert_eq!(type_of_last(source, "type_annotation"), None, "{source}");
    }
}

/// The must-not-move half: with no type parameter shadowing it, the alias still answers.
#[test]
fn without_a_type_parameter_a_member_still_reads_the_outer_alias() {
    for source in [
        "type A = number;\ninterface O { x: A }",
        "type A = number;\ntype O = { x: A };",
        "type A = number;\nabstract class C { abstract x: A }",
    ] {
        assert_eq!(
            type_of_last(source, "type_annotation"),
            Some(Type::Primitive(Primitive::Number)),
            "{source}"
        );
    }
}

/// `function_signature` also carries `parameters`, so making it a scope makes an ambient
/// function's parameters resolvable for the first time.
///
/// A widening beyond the false-positive fix, and a deliberate one: `declare function
/// credit(amount: number)` declares money as a `number` exactly as the non-ambient form
/// does. Asserted here rather than left to be discovered by whoever notices
/// `no-restricted-types` reporting a shape it used to pass over.
#[test]
fn an_ambient_functions_parameter_is_typed() {
    assert_eq!(
        type_of_last("declare function f(a: number): void;", "identifier"),
        Some(Type::Primitive(Primitive::Number))
    );
}
