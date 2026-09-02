//! `lanekeep/no-restricted-types` through the real binary.
//!
//! The per-file behavior lives in `lanekeep-rules/tests/no_restricted_types.rs`, driven
//! straight against the engine through `RuleTester`. That proves the rule's own logic; it
//! does not prove that a project writing this convention in a `lanekeep.json` gets the
//! violation. This is the first shipped rule where `requires: ['types']` decides whether the
//! engine hands it a working `ctx.types` at all — none of that plumbing, nor the config load
//! path, nor the JSON reporter, is exercised by `RuleTester`. This drives the binary instead.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

mod corpus;

use corpus::Corpus;

/// The worked example from the rule's own documentation: money is a `Decimal`, never a
/// `number` or a `string`. Both casings of the name convention are listed —
/// `lanekeep/patterns`' `matches` is case-sensitive, so `'*amount*'` alone would miss a
/// `totalAmount` and only listing `'*Amount*'` too catches it.
const MONEY: &str = "{ conventions: [{ \
     names: ['*amount*', '*Amount*'], \
     forbid: ['number', 'string'], \
     require: { module: 'decimal.js', name: 'Decimal' }, \
     reason: 'number loses precision past 2^53' }] }";

/// A bare `number` on a name the convention governs is reported, at the parameter itself.
#[test]
fn a_bare_number_on_a_governed_name_is_reported() {
    let corpus = Corpus::new(
        "no-restricted-types",
        MONEY,
        &[(
            "src/money.ts",
            "function credit(amount: number) { return amount; }\n",
        )],
    );

    assert_eq!(
        corpus.run(),
        vec!["src/money.ts:1:17 number loses precision past 2^53"],
    );
}

/// The pair the fixture above needs. Without this, a rule that reported on every name the
/// convention matches — whatever its type — would pass the first test just as well: nothing
/// would have shown that the violation depends on the *type* of `amount`, rather than only
/// on its name.
#[test]
fn a_decimal_on_the_same_governed_name_is_a_clean_run() {
    let corpus = Corpus::new(
        "no-restricted-types",
        MONEY,
        &[(
            "src/money.ts",
            "import { Decimal } from 'decimal.js';\n\
             function credit(amount: Decimal) { return amount; }\n",
        )],
    );

    assert_eq!(corpus.run(), Vec::<String>::new());
}
