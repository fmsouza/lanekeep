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
/// A cursor walk rather than indexing: `children` hands back every child in order with no
/// index loop, and it is right whatever type `child_count` answers in — it was `usize`
/// against a `u32` `child` until tree-sitter 0.27, and the cast between them tripped
/// `clippy::cast_possible_truncation`, which this workspace denies.
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
                exported: Some("Decimal".to_owned()),
                module: Some("decimal.js".to_owned()),
            }),
        })
    );
}

/// A renamed import's `Symbol.name` is the local alias, and `exported` is the name the
/// module uses.
///
/// `name` is filled from the reference's own text, deliberately: it is the spelling a
/// message quotes. What used to be dropped one line later — the resolver's
/// `ImportedName::Named("Decimal")` — is now the second field, and this is the fixture where
/// the two differ. `a_named_type_is_nominal` above cannot stand in for it: it imports
/// `Decimal` under its own name, so the use site and the export read identically and an
/// implementation that copied `name` into `exported` would pass it.
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
                exported: Some("Decimal".to_owned()),
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
                exported: None,
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
                exported: None,
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
                exported: Some("Decimal".to_owned()),
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

/// An unrenamed import copies the name rather than leaving `exported` empty.
///
/// The decision, asserted rather than left to fall out of the implementation. `None` here
/// would mean "no alias" and read as the more meaningful contract, and it would make a
/// consumer who forgot `?? name` silently accept every plain import — a false negative on
/// the most ordinary spelling there is, invisible because an ignored requirement only ever
/// removes reports. A copy has no such failure mode and costs one `String`, and it is copied
/// even when nothing was renamed rather than left empty.
#[test]
fn an_imported_name_carries_the_module_it_came_from() {
    assert_eq!(
        symbol_of_use(
            "import { Decimal } from 'decimal.js';\nconst x = Decimal;",
            "Decimal"
        ),
        Some(lanekeep_types::Symbol {
            name: "Decimal".to_owned(),
            exported: Some("Decimal".to_owned()),
            module: Some("decimal.js".to_owned()),
        })
    );
}

/// The shadow pair: the same name, locally declared, carries neither a module nor an
/// exported name — nothing was imported, so there is nothing a module exports it under.
#[test]
fn a_locally_declared_name_carries_no_module() {
    assert_eq!(
        symbol_of_use("class Decimal {}\nconst x = Decimal;", "Decimal"),
        Some(lanekeep_types::Symbol {
            name: "Decimal".to_owned(),
            exported: None,
            module: None,
        })
    );
}

#[test]
fn a_name_nothing_declares_has_no_symbol() {
    assert_eq!(symbol_of_use("const x = missing;", "missing"), None);
}

/// A renamed import carries both names: the alias at the use site, and the name the module
/// exports it under.
///
/// The whole point of the field. `a_renamed_import_s_symbol_name_is_the_local_alias` above
/// pins the first half and passed for the entire life of a `Symbol` that had no second half
/// at all — the resolver's `ImportedName::Named("Decimal")` reached `symbol_at` and was
/// dropped one line later.
#[test]
fn a_renamed_import_carries_the_exported_name_beside_the_alias() {
    assert_eq!(
        symbol_of_use(
            "import { Decimal as Money } from 'decimal.js';\nconst x = Money;",
            "Money"
        ),
        Some(lanekeep_types::Symbol {
            name: "Money".to_owned(),
            exported: Some("Decimal".to_owned()),
            module: Some("decimal.js".to_owned()),
        })
    );
}

/// A default import is exported under the literal name `default`.
///
/// Not the local name, which is chosen freely at the import site and says nothing about the
/// module: `import D from 'm'` and `import Decimal from 'm'` are the same import. `default`
/// is what the module actually exports it as, and Task 4's rule branches on exactly that
/// string to avoid accusing conforming code.
#[test]
fn a_default_import_is_exported_under_the_name_default() {
    assert_eq!(
        symbol_of_use("import Money from 'decimal.js';\nconst x = Money;", "Money"),
        Some(lanekeep_types::Symbol {
            name: "Money".to_owned(),
            exported: Some("default".to_owned()),
            module: Some("decimal.js".to_owned()),
        })
    );
}

/// A namespace import has a module and no exported name at all.
///
/// `import * as d from 'm'` binds the module object, and no single export names it. `None`
/// rather than `"*"`: a sentinel would be a string a comparison could match, and there is
/// nothing here for a name comparison to be right about. This is the discriminating pair for
/// the two tests above — an implementation that always answered `Some` of something, or that
/// defaulted to the use-site name, passes both of them and fails this.
#[test]
fn a_namespace_import_has_a_module_and_no_exported_name() {
    assert_eq!(
        symbol_of_use("import * as d from 'decimal.js';\nconst x = d;", "d"),
        Some(lanekeep_types::Symbol {
            name: "d".to_owned(),
            exported: None,
            module: Some("decimal.js".to_owned()),
        })
    );
}

/// An import specifier may name the export as a string — `import { "Decimal" as D }` — and the
/// exported name is the export's name, not its quoted spelling: a `require` comparing against
/// `Decimal` would otherwise never match it and report conforming code.
#[test]
fn a_string_named_import_specifier_is_exported_without_its_quotes() {
    assert_eq!(
        symbol_of_use(
            "import { \"Decimal\" as D } from 'decimal.js';\nconst x = D;",
            "D"
        ),
        Some(lanekeep_types::Symbol {
            name: "D".to_owned(),
            exported: Some("Decimal".to_owned()),
            module: Some("decimal.js".to_owned()),
        })
    );
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

// --- #208: the six carriers that were still not scopes --------------------------------
//
// The oracle's half. Anchored on a `type_annotation` whose text is `A`, never on a
// `: void` — every source below writes `: A` as its return type, so whichever annotation
// is last in the file resolves the name under test. A fixture that lands on `: void`
// asserts nothing, which is the dead row #207 shipped twice.
//
// #208 and the design both say the reproducer is `typeOf(x) == number`. It is not: with
// `method_signature` outside `SCOPE_KINDS` the walk from `x` finds no declaration at all
// and the oracle answers `None`. The confident wrong answer is reached through the
// *annotation*, which is what these tests read.

/// A type parameter is whatever the call site chose, so the oracle says nothing about it.
///
/// Before this commit the walk escaped past `method_signature` to the outer alias and this
/// answered `Some(Primitive(Number))` — identical in every byte to a declared `number`.
#[test]
fn a_type_parameter_on_a_method_signature_kind_gives_nothing() {
    for source in [
        "type A = number;\ninterface I { m<A>(x: A): A }",
        "type A = number;\nabstract class C { abstract m<A>(x: A): A }",
    ] {
        assert_eq!(type_of_last(source, "type_annotation"), None, "{source}");
    }
}

/// The must-not-move half: with nothing shadowing it, the alias still answers.
#[test]
fn without_a_type_parameter_a_method_signature_kind_reads_the_alias() {
    for source in [
        "type A = number;\ninterface I { m(x: A): A }",
        "type A = number;\nabstract class C { abstract m(x: A): A }",
    ] {
        assert_eq!(
            type_of_last(source, "type_annotation"),
            Some(Type::Primitive(Primitive::Number)),
            "{source}"
        );
    }
}

/// The parameter-side widening, which is the half that is a behavior change rather than a
/// bug fix: these kinds carry `parameters`, so their parameters become resolvable for the
/// first time and an annotated one is typed where it used to give nothing.
#[test]
fn a_parameter_of_a_method_signature_kind_is_typed() {
    for source in [
        "interface I { m(a: number): void }",
        "abstract class C { abstract m(a: number): void }",
    ] {
        assert_eq!(
            type_of_use(source, "a"),
            Some(Type::Primitive(Primitive::Number)),
            "{source}"
        );
    }
}

// --- #208's remaining four ------------------------------------------------------------
//
// `: A` as the return type again, and here it is doing more work than above: the four kinds
// spell their result differently — `call_signature` and `construct_signature` wrap it in a
// `type_annotation`, `constructor_type` and `function_type` hold a bare type after `=>` —
// so which node `type_of_last(_, "type_annotation")` lands on differs by kind. Writing `A`
// in both positions makes the assertion the same one either way.

/// A type parameter is whatever the call site chose in these four kinds too, so the oracle
/// says nothing about it — the same *pair* as `method_signature`'s above, one node kind
/// further from anything with a name. Not the same widening: this half is the bug fix, where
/// the walk used to escape to the outer alias and answer confidently. The widening for these
/// kinds is the parameter-side test below.
#[test]
fn a_type_parameter_in_signature_or_type_position_gives_nothing() {
    for source in [
        "type A = number;\ninterface F { <A>(x: A): A }",
        "type A = number;\ninterface F { new <A>(x: A): A }",
        "type A = number;\ntype F = new <A>(x: A) => A;",
        "type A = number;\ntype F = <A>(x: A) => A;",
    ] {
        assert_eq!(type_of_last(source, "type_annotation"), None, "{source}");
    }
}

/// The must-not-move half: with nothing shadowing it, the alias still answers, in each of
/// the four kinds.
#[test]
fn without_a_type_parameter_signature_or_type_position_reads_the_alias() {
    for source in [
        "type A = number;\ninterface F { (x: A): A }",
        "type A = number;\ninterface F { new (x: A): A }",
        "type A = number;\ntype F = new (x: A) => A;",
        "type A = number;\ntype F = (x: A) => A;",
    ] {
        assert_eq!(
            type_of_last(source, "type_annotation"),
            Some(Type::Primitive(Primitive::Number)),
            "{source}"
        );
    }
}

/// The parameter-side widening for these four kinds: each carries `parameters`, so an
/// annotated one is typed where it used to give nothing.
#[test]
fn a_parameter_in_signature_or_type_position_is_typed() {
    for source in [
        "interface F { (a: number): void }",
        "interface F { new (a: number): F }",
        "type F = new (a: number) => F;",
        "type F = (a: number) => void;",
    ] {
        assert_eq!(
            type_of_use(source, "a"),
            Some(Type::Primitive(Primitive::Number)),
            "{source}"
        );
    }
}

/// Without a provider attached, an imported value still has no type.
///
/// The pair for `an_imported_value_has_no_type_yet` above rather than a replacement for it:
/// that test is now specifically the *no-provider* path, and this names the reason so nobody
/// later reads it as "cross-file resolution does not work". An oracle built by
/// `TypeScriptOracle::new` alone opens no files, by construction — there is nothing attached
/// that could.
#[test]
fn an_oracle_with_no_import_resolution_answers_nothing_for_an_import() {
    assert_eq!(
        type_of_use(
            "import { Decimal } from 'm';\nconst y = Decimal;",
            "Decimal"
        ),
        None
    );
}

/// Type the expression whose source text is exactly `text`.
///
/// `type_of_last(_, "member_expression")` cannot address a chain: `nodes` is pre-order, so
/// the outer `a.b.c` is visited before the inner `a.b`, and `rfind` hands back the inner one.
/// Selecting by exact text names the whole expression a test means.
fn type_of_expr(source: &str, text: &str) -> Option<Type> {
    let tree = parse(source);
    let support = TypeScriptSupport::probe(&TypeScript).expect("TypeScript is supported");
    let oracle = TypeScriptOracle::new(&support, &tree, source);
    let found = nodes(&tree)
        .into_iter()
        .find(|node| source.get(node.byte_range()) == Some(text));
    oracle.type_of(found.unwrap_or_else(|| panic!("no node with text `{text}`")))
}

/// A member off a same-file interface is the member's annotated type.
#[test]
fn a_member_off_a_same_file_interface_is_its_annotated_type() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\nfunction f(o: Order) { return o.amount; }",
            "o.amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// A member off a same-file object-type alias reads the same way.
#[test]
fn a_member_off_a_same_file_object_alias_is_its_annotated_type() {
    assert_eq!(
        type_of_expr(
            "type Order = { amount: bigint };\nfunction f(o: Order) { return o.amount; }",
            "o.amount"
        ),
        Some(Type::Primitive(Primitive::BigInt))
    );
}

/// A public field off a same-file class is its annotated type.
#[test]
fn a_field_off_a_same_file_class_is_its_annotated_type() {
    assert_eq!(
        type_of_expr(
            "class Order { amount: number = 0 }\nfunction f(o: Order) { return o.amount; }",
            "o.amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// An unknown member answers nothing rather than guessing.
#[test]
fn an_unknown_member_answers_nothing() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\nfunction f(o: Order) { return o.missing; }",
            "o.missing"
        ),
        None
    );
}

/// A member off a base the oracle cannot type answers nothing.
#[test]
fn a_member_off_an_untyped_base_answers_nothing() {
    assert_eq!(
        type_of_expr("function f(o) { return o.amount; }", "o.amount"),
        None
    );
}

/// An optional access is the member type or `undefined`.
#[test]
fn an_optional_access_is_the_member_type_or_undefined() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\nfunction f(o: Order) { return o?.amount; }",
            "o?.amount"
        ),
        Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::Undefined),
        ])
    );
}

/// An optional member is the member type or `undefined`, even accessed with a plain dot.
#[test]
fn an_optional_member_is_the_member_type_or_undefined() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount?: number }\nfunction f(o: Order) { return o.amount; }",
            "o.amount"
        ),
        Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::Undefined),
        ])
    );
}

/// A chain reads through several same-file types.
#[test]
fn a_chain_reads_through_same_file_types() {
    assert_eq!(
        type_of_expr(
            "interface Inner { amount: number }\ninterface Outer { inner: Inner }\n\
             function f(o: Outer) { return o.inner.amount; }",
            "o.inner.amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// An optional link taints the whole tail of the chain with `undefined`.
///
/// The `optional_chain` marker sits only on `o?.inner`, but `a?.b.c` short-circuits the whole
/// tail: if `o` is nullish the entire expression is `undefined`, so `.amount` is `number |
/// undefined`, not `number`.
#[test]
fn an_optional_link_taints_the_rest_of_the_chain() {
    assert_eq!(
        type_of_expr(
            "interface Inner { amount: number }\ninterface Outer { inner: Inner }\n\
             function f(o: Outer) { return o?.inner.amount; }",
            "o?.inner.amount"
        ),
        Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::Undefined),
        ])
    );
}

/// A string-literal subscript reads a member exactly as a dot access does.
#[test]
fn a_string_literal_subscript_reads_a_member() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\nfunction f(o: Order) { return o[\"amount\"]; }",
            "o[\"amount\"]"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}

/// A dynamic subscript answers nothing — the oracle has no element-type representation.
#[test]
fn a_dynamic_subscript_answers_nothing() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\nfunction f(o: Order, k: string) { return o[k]; }",
            "o[k]"
        ),
        None
    );
}

/// A locally shadowed type name resolves to the shadow, not the module-level declaration.
///
/// The member walk must resolve a base's type name scope-awarely: a top-level `Box` and a
/// `Box` declared inside a function are different types, and answering the module-level one's
/// member for a value typed by the local one is a confident wrong answer.
#[test]
fn a_member_off_a_shadowed_type_reads_the_shadow() {
    assert_eq!(
        type_of_expr(
            "interface Box { value: number }\n\
             function f() {\n\
             \x20 type Box = { value: string };\n\
             \x20 const b: Box = { value: 's' };\n\
             \x20 return b.value;\n\
             }",
            "b.value"
        ),
        Some(Type::Primitive(Primitive::String))
    );
}

/// A member off a receiver annotated `T | undefined` is the member type or `undefined`.
///
/// In type position `undefined` is a `literal_type`, not a bare `undefined` node — the receiver
/// resolves to its one non-nullish arm, and the whole access carries the `| undefined`.
#[test]
fn a_member_off_a_nullable_receiver_is_or_undefined() {
    assert_eq!(
        type_of_expr(
            "interface Order { amount: number }\n\
             function f(o: Order | undefined) { return o.amount; }",
            "o.amount"
        ),
        Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::Undefined),
        ])
    );
}

/// A nullable intermediate member taints the tail of the chain with `undefined`.
#[test]
fn a_nullable_intermediate_member_taints_the_tail() {
    assert_eq!(
        type_of_expr(
            "interface Amount { cents: number }\n\
             interface Order { amount: Amount | null }\n\
             function f(o: Order) { return o.amount.cents; }",
            "o.amount.cents"
        ),
        Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::Undefined),
        ])
    );
}

/// A member typed as an inline anonymous object is read through: node-based resolution walks
/// the `object_type` directly, so the chain does not need a name at every hop.
#[test]
fn a_chain_reads_through_an_inline_object_member() {
    assert_eq!(
        type_of_expr(
            "interface Outer { inner: { amount: number } }\n\
             function f(o: Outer) { return o.inner.amount; }",
            "o.inner.amount"
        ),
        Some(Type::Primitive(Primitive::Number))
    );
}
