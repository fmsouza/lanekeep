//! `lanekeep/no-restricted-arguments` through the real binary.
//!
//! The per-file behavior lives in `lanekeep-rules/tests/no_restricted_arguments.rs`, driven
//! straight against the engine through `RuleTester`. That proves the rule's own logic; it does
//! not prove that a project writing this convention in a `lanekeep.json` gets the violation —
//! the config load path, the `requires: ['types']` plumbing that decides whether the engine
//! hands the rule a working `ctx.types` at all, and the reporter, none of which `RuleTester`
//! exercises. This drives the binary instead.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

mod corpus;

use corpus::Corpus;

/// The worked example from the rule's own documentation: money is constructed from a string,
/// never a `number`.
const MONEY: &str = "{ restrictions: [{ \
     call: { module: 'decimal.js', name: 'Decimal' }, \
     forbid: ['number'], \
     reason: 'construct a Decimal from a string, not a float' }] }";

/// #185 §A.1's own example, verbatim, carrying both the inline form it named and the named
/// form #203 measured as also uncaught by `no-restricted-types`. Both report under this rule:
/// the inline `parseFloat(row.amount)` types as `number` directly, and `amount` in the second
/// function resolves — through the oracle's declarator path — to a declarator whose initializer
/// is the same `parseFloat(row.amount)` call, so it types as `number` too. Asserting only one
/// of the two would let a rule that stopped resolving identifiers back to their declarators
/// pass this test while silently losing the case the brief measured.
#[test]
fn the_inline_and_named_forms_are_both_reported() {
    let corpus = Corpus::new(
        "no-restricted-arguments",
        MONEY,
        &[(
            "src/money.ts",
            "import { Decimal } from 'decimal.js';\n\
             \n\
             export function load(row: { amount: string }) {\n\
             \x20 return new Decimal(parseFloat(row.amount));\n\
             }\n\
             \n\
             export function named(row: { amount: string }) {\n\
             \x20 const amount = parseFloat(row.amount);\n\
             \x20 return new Decimal(amount);\n\
             }\n",
        )],
    );

    assert_eq!(
        corpus.run(),
        vec![
            "src/money.ts:4:22 construct a Decimal from a string, not a float",
            "src/money.ts:9:22 construct a Decimal from a string, not a float",
        ],
    );
}

/// The pair the fixture above needs: the same two call sites, with `row.amount` passed
/// directly instead of through `parseFloat`. `row.amount` is a member expression the oracle
/// cannot type, so both calls are silent. Without this, a rule that reported on every call to
/// a matching callee regardless of its argument's type would pass the test above just as well.
#[test]
fn the_same_shape_with_no_forbidden_conversion_is_a_clean_run() {
    let corpus = Corpus::new(
        "no-restricted-arguments",
        MONEY,
        &[(
            "src/money.ts",
            "import { Decimal } from 'decimal.js';\n\
             \n\
             export function load(row: { amount: string }) {\n\
             \x20 return new Decimal(row.amount);\n\
             }\n\
             \n\
             export function named(row: { amount: string }) {\n\
             \x20 const amount = row.amount;\n\
             \x20 return new Decimal(amount);\n\
             }\n",
        )],
    );

    assert_eq!(corpus.run(), Vec::<String>::new());
}

/// The alias, through this binary rather than only through `RuleTester` — placed here because
/// that is where the sibling rule's alias false positive was actually found: measured through
/// the real config-load-and-report path, not through direct engine invocation. Deleting the
/// `ctx.resolvesToImport` guard in favor of comparing `m.callee`'s text would accept this file,
/// since the call site never spells `Decimal`.
#[test]
fn an_aliased_import_is_reported_through_the_binary() {
    let corpus = Corpus::new(
        "no-restricted-arguments",
        MONEY,
        &[(
            "src/alias.ts",
            "import { Decimal as Money } from 'decimal.js';\n\
             \n\
             export function convert(x: string) {\n\
             \x20 return new Money(parseFloat(x));\n\
             }\n",
        )],
    );

    assert_eq!(
        corpus.run(),
        vec!["src/alias.ts:4:20 construct a Decimal from a string, not a float"],
    );
}
