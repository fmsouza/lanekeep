//! `lanekeep/no-restricted-arguments`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

/// The worked example from the documentation: money is constructed from a string, never a
/// `number`.
const MONEY: &str = "{ restrictions: [{ \
     call: { module: 'decimal.js', name: 'Decimal' }, \
     forbid: ['number'], \
     reason: 'construct a Decimal from a string, not a float' }] }";

fn tester(options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-restricted-arguments").expect("the rule ships");
    RuleTester::configured("no-restricted-arguments", source, options)
        .expect("builds")
        .with_builtins(lanekeep_rules::source)
}

/// The motivating case. Deleting the whole `check` body (or the `resolvesToImport` guard) would
/// make this test the first to fail.
#[test]
fn a_forbidden_type_on_the_default_argument_is_reported() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(row.amount));\n",
            &[(2, 13)],
        )
        .expect("`parseFloat` gives a number and MONEY forbids one at position 0");
}

/// The half that denies a rule reporting on every argument of a matching callee whatever its
/// type. Without this, a rule that ignored the type entirely would still pass the test above.
#[test]
fn an_untypeable_argument_on_a_matching_callee_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(row.amount);\n",
        )
        .expect("`row.amount` is a member expression the oracle cannot type");
}

/// The half that denies a rule reporting on every call named `parseFloat` regardless of callee.
#[test]
fn a_forbidden_argument_on_an_unmatched_callee_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Other } from 'somewhere-else';\n\
             new Other(parseFloat(x));\n",
        )
        .expect("`Other` does not resolve to decimal.js's Decimal");
}

/// A plain call, no `new` — the `call_expression` half of the query, pinned on its own.
#[test]
fn a_plain_call_is_reported_too() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             Decimal(parseFloat(x));\n",
            &[(2, 9)],
        )
        .expect(
            "the call_expression clause captures a bare Decimal(...) just like new Decimal(...)",
        );
}

/// The alias — precision `no-restricted-types` cannot reach, because its `require` had to give
/// up name comparison for the same reason.
#[test]
fn an_aliased_import_of_the_restricted_callee_is_reported() {
    tester(MONEY)
        .reports_at(
            "import { Decimal as Money } from 'decimal.js';\n\
             new Money(parseFloat(x));\n",
            &[(2, 11)],
        )
        .expect("Money resolves to decimal.js's Decimal through the import, not through its text");
}

/// A regression pin for the reported position on this source, not a proof that position 1 is
/// never examined: `check` returns after its first report, so a broken implementation that
/// defaulted to checking *every* position (`args.map((_, i) => i)`) would still report here, at
/// this same position 0, and this test alone cannot tell the two apart — confirmed, that
/// mutation fails `argument_all_reaches_a_position_the_default_misses` and
/// `argument_one_reaches_it_alone` and not this test. Those two tests are what actually deny
/// "every position is checked by default"; this one only pins where the report lands.
#[test]
fn the_radix_is_silent_by_default() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(a), 10);\n",
            &[(2, 13)],
        )
        .expect("position 0 is reported; the 10 at position 1 is not what fired this report");
}

/// `argument: 'all'` reaches a position the default misses. One source, two configurations,
/// opposite outcomes — this pair is the only thing that shows `argument` is read at all.
///
/// `row.amount` (position 0, an *annotated* member expression — `row: { amount: string }`) is a
/// member expression the oracle answers `undefined` about even though the surrounding parameter
/// carries a type; `10` (position 1) types as `number`. Under MONEY's default position this
/// source is accepted, and under `argument: 'all'` it is reported at the `10`.
///
/// The annotation matters: without it the fixture would only show "the oracle cannot type an
/// undeclared identifier's member", which stays true even if binding resolution later improves
/// and starts answering unannotated members. With it, the property under test is the real one —
/// the oracle stays silent on a member expression's type even when the base is declared.
#[test]
fn argument_all_reaches_a_position_the_default_misses() {
    let source = "import { Decimal } from 'decimal.js';\n\
                  function use(row: { amount: string }) { new Decimal(row.amount, 10); }\n";

    tester(MONEY).accepts(source).expect(
        "position 0, row.amount, types as undefined under the default even though row is annotated",
    );

    let all = "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number'], \
         argument: 'all', \
         reason: 'construct a Decimal from a string, not a float' }] }";
    tester(all)
        .reports_at(source, &[(2, 65)])
        .expect("'all' reaches position 1, the 10, which types as number");
}

/// `argument: 1` reaches it alone, on both fixtures above — and on test `the_radix_is_silent_by_default`'s
/// source, where the default would have reported at position 0 instead. The column assertion is
/// what tells this apart from the default: both report on the same source, at different columns.
#[test]
fn argument_one_reaches_it_alone() {
    let named_position = "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number'], \
         argument: 1, \
         reason: 'construct a Decimal from a string, not a float' }] }";

    tester(named_position)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             function use(row: { amount: string }) { new Decimal(row.amount, 10); }\n",
            &[(2, 65)],
        )
        .expect("argument: 1 reaches the 10 even though position 0 is untypeable");

    tester(named_position)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(a), 10);\n",
            &[(2, 28)],
        )
        .expect(
            "argument: 1 reports at the 10, column 28 — not at position 0's column 13, \
                 which is what the default in `the_radix_is_silent_by_default` reports",
        );
}

/// A leading block comment does not shift the check. Deleting the `filter` would make this test
/// pass silently only if it asserted acceptance — it asserts a report instead, which the
/// unfiltered rule cannot produce (it would ask the oracle about the comment, get `undefined`,
/// and go silent).
#[test]
fn a_leading_comment_does_not_shift_the_check() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(/* cents */ parseFloat(x));\n",
            &[(2, 25)],
        )
        .expect("the comment filter keeps parseFloat(x) at position 0");
}

/// A trailing comment does not disturb position 0 — but note this is a weaker claim than the
/// leading-comment test above. Deleting the `filter` fails
/// `a_leading_comment_does_not_shift_the_check` but **not** this test: an unfiltered rule still
/// reads `parseFloat(x)` at index 0 here, because the comment trails it at index 1, so this
/// fixture cannot show the filter matters — it only pins that a trailing comment does not
/// disturb the report.
#[test]
fn a_trailing_comment_does_not_shift_the_check() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(x) /* rounded */);\n",
            &[(2, 13)],
        )
        .expect("a trailing comment does not move position 0's report");
}

/// A nested conversion is judged on its immediate type, not on the value that fed it — a
/// deliberate choice: the precision loss in `String(parseFloat(s))` happened one call earlier,
/// which is dataflow this rule does not attempt.
#[test]
fn a_nested_conversion_is_judged_on_its_immediate_type() {
    let forbid_number_only = "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number'], \
         reason: 'r' }] }";
    tester(forbid_number_only)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(String(x));\n",
        )
        .expect("String(x) types as string, which forbid: ['number'] does not name");

    let forbid_both = "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number', 'string'], \
         reason: 'r' }] }";
    tester(forbid_both)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(String(x));\n",
            &[(2, 13)],
        )
        .expect("the same call's immediate type, string, is now on the forbid list");
}

/// A spread is silent: `spread_element` types as `undefined`.
#[test]
fn a_spread_argument_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(...xs);\n",
        )
        .expect(
            "a spread_element types as undefined and the oracle's answer is not second-guessed",
        );
}

/// A zero-argument construction is silent — `args[0]` is `undefined` and the position guard
/// short-circuits before the oracle is ever asked.
#[test]
fn a_zero_argument_construction_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal();\n",
        )
        .expect("there is no argument at position 0 to type at all");
}

/// A union member fires: a bare `number` is still reachable through `number | Decimal`.
#[test]
fn a_forbidden_union_member_is_reported() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             function use(v: number | Decimal) { new Decimal(v); }\n",
            &[(2, 49)],
        )
        .expect("number is one of the union's members and it is forbidden");
}

/// A union with no forbidden member is silent — optional money is still money.
#[test]
fn a_union_with_no_forbidden_member_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             function use(v: Decimal | undefined) { new Decimal(v); }\n",
        )
        .expect("neither union member is a forbidden primitive");
}

/// An empty `restrictions` list reports nothing, over the motivating case's own source.
#[test]
fn an_empty_restrictions_list_reports_nothing() {
    tester("{}")
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(row.amount));\n",
        )
        .expect("a restriction nobody wrote forbids nothing");
}

/// A nominal-typed argument is accepted: `isForbidden`'s fallthrough (neither `primitive` nor
/// `union` is set) must stay `false`, never `true`. Nothing else in this file reaches that
/// branch — the union tests return before it, and the untypeable-argument test short-circuits
/// on `undefined` before `isForbidden` is even called — so without this test a rule that
/// treated every nominal type as forbidden would accuse its own `card.examples.good` shape
/// (`new Decimal(row.amount)` typed through a `Decimal`-annotated parameter) and nothing here
/// would notice.
#[test]
fn a_nominal_typed_argument_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             function use(v: Decimal) { new Decimal(v); }\n",
        )
        .expect("a nominal type is neither a forbidden primitive nor a union carrying one");
}

/// A restriction with no `forbid` list forbids nothing. `restriction.forbid ?? []` is what
/// keeps a `forbid`-less restriction from being read as "forbid everything" — without the `??
/// []` default (e.g. a stray `?? ['number']`), this exact restriction would report on
/// `parseFloat(x)` and this test is the only thing that would notice.
#[test]
fn a_restriction_with_no_forbid_list_reports_nothing() {
    tester("{ restrictions: [{ call: { module: 'decimal.js', name: 'Decimal' } }] }")
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(x));\n",
        )
        .expect("an absent forbid list is an empty one, not an implicit forbid-all");
}

/// A restriction naming no `call` at all is silent — the `if (call === undefined) continue`
/// guard, pinned on its own. Nothing else in this file omits `call`, so nothing else would
/// notice this guard being deleted; without it, reading `call.module` on the next line would
/// throw instead of skipping the restriction.
#[test]
fn a_restriction_with_no_call_is_silent() {
    tester("{ restrictions: [{ forbid: ['number'] }] }")
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(x));\n",
        )
        .expect("a restriction with nothing to match against matches nothing");
}

/// `argument: 'all'` still reports only once per call, even when two positions are both
/// forbidden — `check` returns after its first report. Deleting that early `return` is
/// invisible to every other test here, because none of them gives a matching call two
/// forbidden positions to find; this fixture does, and asserts exactly one violation rather
/// than two.
#[test]
fn argument_all_reports_once_even_with_two_forbidden_positions() {
    let all = "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number'], \
         argument: 'all', \
         reason: 'r' }] }";
    tester(all)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             new Decimal(parseFloat(a), parseFloat(b));\n",
            &[(2, 13)],
        )
        .expect(
            "both positions are forbidden, but the rule reports once and returns, at the first",
        );
}

/// The message falls back to the generic line when `reason` is absent.
#[test]
fn the_reported_message_falls_back_with_no_reason() {
    tester(
        "{ restrictions: [{ \
         call: { module: 'decimal.js', name: 'Decimal' }, \
         forbid: ['number'] }] }",
    )
    .reports_messages(
        "import { Decimal } from 'decimal.js';\n\
         new Decimal(parseFloat(x));\n",
        &["this argument type is restricted here"],
    )
    .expect("with no reason of its own the restriction has only the generic fallback");
}
