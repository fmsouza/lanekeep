//! `lanekeep/no-restricted-types`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

/// The worked example from the documentation: money is a `Decimal`, never a `number`.
const MONEY: &str = "{ conventions: [{ \
     names: ['*amount*', '*balance*'], \
     forbid: ['number', 'string'], \
     require: { module: 'decimal.js', name: 'Decimal' }, \
     reason: 'number loses precision past 2^53' }] }";

fn tester(options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-restricted-types").expect("the rule ships");
    RuleTester::configured("no-restricted-types", source, options)
        .expect("builds")
        .with_builtins(lanekeep_rules::source)
}

#[test]
fn a_forbidden_primitive_on_a_matching_name_is_reported() {
    tester(MONEY)
        .reports_at(
            "function credit(amount: number) { return amount; }\n",
            &[(1, 17)],
        )
        .expect("`amount` is money and money is not a number");
}

/// The half that denies a rule reporting on every matched name whatever its type.
#[test]
fn the_required_type_on_a_matching_name_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             function credit(amount: Decimal) { return amount; }\n",
        )
        .expect("a Decimal amount is what the convention asks for");
}

/// The half that denies a rule ignoring `names` and reporting every forbidden primitive.
#[test]
fn a_forbidden_primitive_on_a_name_outside_the_convention_is_accepted() {
    tester(MONEY)
        .accepts("function retry(retries: number) { return retries; }\n")
        .expect("`retries` is not money");
}

// --- the casing trap, as a pair ---
//
// `names` is matched with `lanekeep/patterns`' case-sensitive glob, and `docs/built-in-rules.md`
// calls a pattern that silently matches nothing the worst outcome this rule has: nothing throws,
// nothing warns, and the run reads exactly like a conforming codebase. `totalAmount` is the
// spelling a real project writes, and neither fixture file exercised it in either direction.
//
// Neither half means anything alone. On its own the silent one passes against a rule that
// reports nothing at all, and the reporting one passes against a rule whose matching is
// case-*insensitive*. Together they say the casing is what decided it.

/// The trap itself: one casing in the convention, the other in the code, and silence.
#[test]
fn a_lowercase_only_convention_silently_misses_a_camel_case_name() {
    tester("{ conventions: [{ names: ['*amount*'], forbid: ['number'], reason: 'money' }] }")
        .accepts("function report(totalAmount: number) { return totalAmount; }\n")
        .expect("`*amount*`'s lowercase `a` never matches the capital one in `totalAmount`");
}

/// The half that makes the one above an assertion rather than a coincidence: the identical
/// source, reported the moment the convention lists the casing the code actually uses.
#[test]
fn a_convention_listing_the_camel_case_spelling_reports_it() {
    tester("{ conventions: [{ names: ['*Amount*'], forbid: ['number'], reason: 'money' }] }")
        .reports_at(
            "function report(totalAmount: number) { return totalAmount; }\n",
            &[(1, 17)],
        )
        .expect("`*Amount*` is the spelling that catches `totalAmount`");
}

/// The shadow case. `require` matches on the module a symbol came from, never on its name,
/// so a local class sharing the name is not the imported type and is still a violation.
#[test]
fn a_local_type_sharing_the_required_name_is_still_reported() {
    tester(MONEY)
        .reports_at(
            "class Decimal {}\nfunction credit(amount: Decimal) { return amount; }\n",
            &[(2, 17)],
        )
        .expect("a local Decimal is not decimal.js's");
}

/// The other half of "matched on the module, never on the name", and the one the shadow case
/// above cannot stand in for: it is killed by the module comparison alone, so it stayed green
/// for the whole life of a rule that also compared `symbol.name` and reported this.
///
/// `symbol.name` is the *use-site* name — the oracle fills it from the node's own text and
/// discards the exported one — so an aliased import of exactly the required type read as a
/// different type and was accused with a message about `number`. Conforming code, reported.
/// Pinned here because it is the one failure this rule's whole design forbids.
#[test]
fn a_renamed_import_of_the_required_type_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal as Money } from 'decimal.js';\n\
             function credit(amount: Money) { return amount; }\n",
        )
        .expect("an alias of decimal.js's Decimal is still decimal.js's Decimal");
}

/// The cost of the fixture above, asserted so nobody discovers it as a surprise: matching on
/// the module alone accepts *any* export of that module, so a convention requiring `Decimal`
/// takes a `Big` from the same package. A false negative, and the deliberate trade — the
/// alternative is the false positive the test above pins.
#[test]
fn a_different_export_of_the_required_module_is_accepted_too() {
    tester(MONEY)
        .accepts(
            "import { Big } from 'decimal.js';\n\
             function credit(amount: Big) { return amount; }\n",
        )
        .expect("the module is all that is matched, so a sibling export passes");
}

/// The oracle's initializer path, reached from a declaration-site identifier.
///
/// This is also the pin for #203's *named* form: `no-restricted-types` reaches
/// `new Decimal(parseFloat(row.amount))`'s inline argument only by following `amount` back to
/// its declarator, never at the call site itself — the case `lanekeep/no-restricted-arguments`
/// exists to catch the *inline* form this rule cannot. Removing this test would silently drop
/// the coverage the new rule was required to add to, never replace: mutation testing found no
/// way to distinguish a differently-scoped or differently-named copy of this fixture from this
/// one, because neither the declarator path nor the call-typing path branches on scope or on
/// the parenthesized argument's own text — `crates/lanekeep-types/src/oracle.rs:196-211` types
/// a builtin call by name alone, without inspecting its argument — so a second fixture in this
/// shape would only duplicate this one, not add to it.
#[test]
fn a_local_typed_through_its_initializer_is_reported() {
    tester(MONEY)
        .reports_at("const amount = parseFloat(raw);\n", &[(1, 7)])
        .expect("parseFloat gives a number and the name says money");
}

/// `undefined` is a first-class answer and the rule stays silent on it.
///
/// Asserted as *expected* rather than left to fall out: a rule that reported on `undefined`
/// would still pass every other fixture in this file, and would accuse code the oracle
/// simply could not read.
#[test]
fn a_value_the_oracle_cannot_type_is_accepted() {
    tester(MONEY)
        .accepts("const amount = maybe ?? fallback;\n")
        .expect("`??` is outside the operator table, so the oracle says nothing");
}

/// `require` is optional — "never a raw primitive here" is a legitimate convention.
#[test]
fn a_convention_with_no_required_type_still_forbids() {
    tester("{ conventions: [{ names: ['*Id*'], forbid: ['number'], reason: 'ids are opaque' }] }")
        .reports_at(
            "function find(userId: number) { return userId; }\n",
            &[(1, 15)],
        )
        .expect("the convention forbids a number id even with nothing to require");
}

#[test]
fn a_rule_with_no_conventions_reports_nothing() {
    tester("{ conventions: [] }")
        .accepts("function credit(amount: number) { return amount; }\n")
        .expect("a convention nobody wrote forbids nothing");
}

// --- the union case: neither `primitive` nor `symbol` is set, and it needs its own answer ---
//
// A union reports iff any of its members is a forbidden primitive, decided independently of
// the nominal branch. Both directions are asserted, because the wrong fix in either direction
// looks locally reasonable: `continue`-ing past every union silently accepts a bare `number`
// hiding in `number | Decimal`, and falling through to the nominal check below reports on
// every union (unions carry no `symbol` of their own), which would break `Decimal | undefined`
// — optional money that conforms.

/// The false positive a union with no branch of its own falls into: `type.symbol` is unset for
/// a union exactly as it is for an unattributable nominal, so the nominal check below cannot
/// tell "optional money" from "money of an unknown type" unless the union is handled first.
#[test]
fn a_union_of_the_required_type_and_undefined_is_accepted() {
    tester(MONEY)
        .accepts(
            "import { Decimal } from 'decimal.js';\n\
             function credit(amount: Decimal | undefined) { return amount; }\n",
        )
        .expect("optional money is still money; no member of the union is a forbidden primitive");
}

/// The other direction: a union can still be a bare forbidden primitive at run time, so the
/// naive fix for the false positive above — skipping every union — would silently accept this.
#[test]
fn a_forbidden_member_of_a_union_is_reported() {
    tester(MONEY)
        .reports_at(
            "import { Decimal } from 'decimal.js';\n\
             function credit(amount: number | Decimal) { return amount; }\n",
            &[(2, 17)],
        )
        .expect("`number` is one of the union's members and it is forbidden");
}

/// The membership check on its own: `boolean` is a primitive, just not one MONEY forbids.
/// Deleting the `forbid` check would report on every primitive regardless of the list.
#[test]
fn an_unforbidden_primitive_on_a_matching_name_is_accepted() {
    tester(MONEY)
        .accepts("function credit(amount: boolean) { return amount; }\n")
        .expect("boolean is not in MONEY's forbid list");
}

/// An unresolvable, global or ambient nominal type — the oracle cannot attribute it to a
/// symbol at all, so it carries only `text`. Flagged in the design as the decision most
/// likely to be reversed: a governed value whose type cannot be established is not evidence
/// the convention is met, so it is reported rather than given the benefit of the doubt.
#[test]
fn an_unattributable_nominal_type_is_reported() {
    tester(MONEY)
        .reports_at(
            "function credit(amount: Date) { return amount; }\n",
            &[(1, 17)],
        )
        .expect("`Date` is ambient and the oracle cannot attribute it to a symbol");
}

/// `require` is optional, and a convention that omits it must not blow up meeting a nominal
/// type — reading `convention.require.module` throws when `require` itself is `undefined`,
/// which is exactly the guard the line above this assertion protects.
#[test]
fn a_convention_with_no_required_type_is_silent_on_a_nominal_type() {
    tester("{ conventions: [{ names: ['*Id*'], forbid: ['number'], reason: 'ids are opaque' }] }")
        .accepts("class OrderId {}\nfunction find(orderId: OrderId) { return orderId; }\n")
        .expect("a nominal type is not a forbidden primitive, and there is nothing to require");
}

/// `reasonFor` and the per-report message override actually reach the output, rather than
/// the card's generic `message`.
#[test]
fn the_reported_message_is_the_conventions_reason() {
    tester(MONEY)
        .reports_messages(
            "function credit(amount: number) { return amount; }\n",
            &["number loses precision past 2^53"],
        )
        .expect("the convention's own reason replaces the card's generic message");
}

/// The same override on a convention with no `require`, so the message is not tied to the
/// `require`-derived fallback text `reasonFor` also knows how to build.
#[test]
fn the_reported_message_is_the_conventions_reason_with_no_required_type() {
    tester("{ conventions: [{ names: ['*Id*'], forbid: ['number'], reason: 'ids are opaque' }] }")
        .reports_messages(
            "function find(userId: number) { return userId; }\n",
            &["ids are opaque"],
        )
        .expect("the reason is carried even when the convention has nothing to require");
}

// --- `reasonFor`'s two fallbacks ---
//
// Every convention in this file and in the CLI's supplies a `reason`, so only the first of
// `reasonFor`'s three branches was reached. Both fallbacks worked and neither was pinned, which
// is the shape a refactor deletes without turning anything red: `reason` is optional in the
// options schema, so a project that omits it is on one of these two lines.

/// No `reason`, but something to require: the message names the replacement type.
#[test]
fn a_convention_with_no_reason_falls_back_to_naming_the_required_type() {
    tester(
        "{ conventions: [{ names: ['*amount*'], forbid: ['number'], \
         require: { module: 'decimal.js', name: 'Decimal' } }] }",
    )
    .reports_messages(
        "function credit(amount: number) { return amount; }\n",
        &["use Decimal from decimal.js"],
    )
    .expect("with no reason of its own the convention's `require` is what there is to say");
}

/// Neither `reason` nor `require` — the last resort, and the only message the rule can build
/// with nothing at all to go on.
#[test]
fn a_convention_with_neither_reason_nor_required_type_falls_back_to_a_generic_message() {
    tester("{ conventions: [{ names: ['*amount*'], forbid: ['number'] }] }")
        .reports_messages(
            "function credit(amount: number) { return amount; }\n",
            &["this type is restricted on a value the convention governs"],
        )
        .expect("nothing to quote and nothing to require leaves the generic line");
}

/// The `optional_parameter` query clause, pinned on its own. Without it an offending optional
/// parameter is never captured at all, which reads exactly like a file with nothing wrong.
#[test]
fn an_optional_parameter_is_reported_too() {
    tester(MONEY)
        .reports_at(
            "function credit(amount?: number) { return amount; }\n",
            &[(1, 17)],
        )
        .expect("the optional_parameter clause captures amount just like the required one");
}
