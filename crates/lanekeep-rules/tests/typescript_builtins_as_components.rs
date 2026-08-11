//! `no-default-export` and `no-restricted-imports` as the components they actually ship as.
//!
//! # Why this file exists, and what it is not
//!
//! The acceptance test for compiling the four TypeScript built-ins was that their existing test
//! files pass unmodified. For two of the four that is literally what happened:
//! `crates/lanekeep-cli/tests/no_circular_imports.rs` and `.../no_unused_exports.rs` drive the
//! real binary, which resolves `lanekeep/<name>` through the embedded table and runs the
//! component.
//!
//! The other two do not reach it. `tests/no_default_export.rs` and
//! `tests/no_restricted_imports.rs` ask [`lanekeep_rules::source`] for the TypeScript the rule
//! was authored in and run *that* through `RuleTester`, which generates a `lanekeep.config.ts`
//! and evaluates it in QuickJS — the engine the rule was written against and no longer the one
//! it ships on. Both files are frozen, so they say what they said before the migration and
//! nothing about the artifact. Until this file, the two rules had **no behavioral coverage as
//! components at all**: their positions, their message text and their option handling were
//! pinned only against the source, under the old engine.
//!
//! **This is a second encoding of those files' cases and not a substitute for them**, which
//! `docs/authoring-rust-rules.md` warns about in as many words: two independently written test
//! files can each look sensible and assert different things, and a case only one of them has is
//! a case the other is not held to. The frozen files remain the exhaustive statement of what
//! each rule does. What is here is the subset that would catch a component which is a different
//! program from the source beside it — wrong positions, wrong message, options not arriving —
//! chosen because every case costs a compile of a 12.4 MiB artifact.
//!
//! # One tester per test, deliberately
//!
//! Measured 2026-08-11 on the release profile: constructing a built-in tester is ~1.6 ms, its
//! **first** `run` is ~6.5 s because that is where the shared component is compiled, and every
//! later `run` on the same tester is ~0.2 s because `lanekeep-testkit` names the throwaway
//! project as the artifact root and the second load maps what the first wrote. So cases are
//! grouped onto as few testers as the configurations allow, and each tester sits in its own
//! `#[test]` so the compiles overlap rather than queue.
//!
//! What this file does *not* need to assert is that the artifact is built from these sources —
//! `crates/lanekeep-wasm/tests/fixture_currency.rs` does that against a digest manifest — or
//! that the name-to-index table agrees with the component's own enumeration, which is
//! `tests/component_rules.rs`. Neither of those can see behavior, and this cannot see staleness.

// No crate-level `expect` for `clippy::expect_used` and `clippy::panic`, unlike the frozen files
// beside this one: everything here lives inside a `#[test]`, which is exactly what
// `clippy.toml`'s allow-*-in-tests already grants. Restating a grant that is already in force is
// an unfulfilled expectation, and `-D warnings` makes that an error. The frozen files need it
// because they hold a `fn tester()` helper, which is neither a `#[test]` nor a `#[cfg(test)]`
// module; this file builds its testers inline instead.

use lanekeep_testkit::RuleTester;

/// Every restriction shape the frozen file separates onto its own tester, in one config.
///
/// One tester is one `lanekeep.json`, and a tester is six and a half seconds. Combining them
/// costs a little independence — a bug in glob matching now fails a test whose name mentions
/// several things — and buys the difference between one compile and five. The frozen file is
/// where each shape is isolated.
const RESTRICTIONS: &str = r#"{
    "restrictions": [
        { "module": "lodash" },
        { "module": "lodash*" },
        { "module": "@internal/*" },
        { "module": "stripe", "from": ["subject/*"],
          "reason": "route it through @app/payments" }
    ]
}"#;

#[test]
fn no_default_export_as_a_component_reports_what_the_module_did() {
    let tester = RuleTester::for_built_in("no-default-export", "ts", lanekeep_rules::component)
        .expect("builds");

    tester
        .accepts("export function parse() {}\nexport const VERSION = 1;\n")
        .expect("named exports are the point");
    tester
        .reports_at("export default function parse() {}\n", &[(1, 1)])
        .expect("the declaration form");
    tester
        .reports_at(
            "const parse = () => {};\nexport default parse;\n",
            &[(2, 1)],
        )
        .expect("the expression form");
    tester
        .reports_at("export default class Parser {}\n", &[(1, 1)])
        .expect("the class form");
    tester
        .reports_at(
            "export default function a() {}\nexport default function b() {}\n",
            &[(1, 1), (2, 1)],
        )
        .expect("a handler that stopped at the first would pass every other case here");

    // The three that carry a column other than 1. A component reporting a node it resolved
    // differently would still land on line 1 and would not land on column 10 or 19.
    tester
        .reports_at("export { default } from './parse';\n", &[(1, 10)])
        .expect("a re-exported default is reported at the token that has to change");
    tester
        .reports_at(
            "function parse() {}\nexport { parse as default };\n",
            &[(2, 19)],
        )
        .expect("the alias is what gets exported");
    tester
        .accepts("export { default as parse } from './parse';\n")
        .expect("de-defaulting an import is the remedy, not the violation");
}

#[test]
fn no_default_export_as_a_component_carries_the_same_card() {
    // The card is produced by `metadata`, which for a component is the guest answering for
    // itself rather than a `defineRule` literal being read out of a module. So this is the one
    // assertion here that exercises a mechanism the QuickJS arm does not have.
    let violations = RuleTester::for_built_in("no-default-export", "ts", lanekeep_rules::component)
        .expect("builds")
        .run("export default function parse() {}\n")
        .expect("runs");

    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert_eq!(violation.rule_id.to_string(), "lanekeep/no-default-export");
    assert!(
        violation.message.contains("default export"),
        "message does not name the problem: {}",
        violation.message
    );
    assert!(
        violation.remediation.contains("named export"),
        "remediation does not name the fix: {}",
        violation.remediation
    );
}

#[test]
fn no_restricted_imports_as_a_component_reads_its_options() {
    let tester = RuleTester::for_built_in_configured(
        "no-restricted-imports",
        "ts",
        lanekeep_rules::component,
        RESTRICTIONS,
    )
    .expect("builds");

    tester
        .accepts("import { join } from 'node:path';\n")
        .expect("nothing restricts node:path");
    tester
        .reports_at("import merge from 'lodash';\n", &[(1, 1)])
        .expect("a plain module restriction");
    tester
        .reports_at("import x from '@internal/db/client';\n", &[(1, 1)])
        .expect("a glob spans separators");
    tester
        .reports_at(
            "import merge from 'lodash';\nimport Stripe from 'stripe';\nimport { join } from 'node:path';\n",
            &[(1, 1), (2, 1)],
        )
        .expect("every restricted import, and only those");

    // `lodash` matches two entries. Reporting twice would double-count one line, and it is the
    // deduplication rather than the matching that a re-compiled rule could plausibly lose.
    let violations = tester.run("import merge from 'lodash';\n").expect("runs");
    assert_eq!(
        violations.len(),
        1,
        "overlapping restrictions reported more than once: {violations:?}"
    );

    let violations = tester.run("import Stripe from 'stripe';\n").expect("runs");
    let [violation] = violations.as_slice() else {
        panic!("expected exactly one violation, got {}", violations.len());
    };
    assert_eq!(
        violation.rule_id.to_string(),
        "lanekeep/no-restricted-imports"
    );
    assert!(
        violation.message.contains("stripe"),
        "message does not name the module: {}",
        violation.message
    );
    assert!(
        violation.message.contains("route it through @app/payments"),
        "the configured reason did not reach the message: {}",
        violation.message
    );
}

#[test]
fn no_restricted_imports_as_a_component_restricts_nothing_by_default() {
    // The inert direction, and it is the half that can fail silently. An option that never
    // arrives makes a rule report *more*, so the paired assertions above would still pass
    // against a component ignoring `configure` entirely if this one did not exist: what
    // distinguishes "the options arrived" from "the rule is unconditionally strict" is a
    // configuration under which it reports nothing.
    RuleTester::for_built_in_configured(
        "no-restricted-imports",
        "ts",
        lanekeep_rules::component,
        "{}",
    )
    .expect("builds")
    .accepts("import merge from 'lodash';\n")
    .expect("an unconfigured restriction list restricts nothing");
}
