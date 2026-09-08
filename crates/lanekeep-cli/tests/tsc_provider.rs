//! The `tsc` provider, end to end, over the same fixtures the built-in one answers.
//!
//! Gated on `packages/lanekeep/node_modules/typescript`: a test that finds it absent prints
//! why and returns, so this suite is real wherever the authoring package is installed — Node
//! 24 and `typescript` 5.9.3 in `packages/lanekeep`'s devDependencies — and skipped where it
//! is not. The **package** rather than `.bin/tsc`, because the driver loads the package
//! through `createRequire` and never runs the binary; a machine with the shim and no package
//! would skip nothing and fail. That is the gate `crates/lanekeep-types/src/tsc/driver.test.mjs`
//! and the engine's own `tsc_provider` module already use, and the skip prints rather than
//! being silent because a suite that quietly checks nothing is the failure this project keeps
//! finding.
//!
//! **Why this lives in `lanekeep-cli`'s tests rather than `lanekeep-types`'.** It drives
//! `RuleTester`, and `lanekeep-testkit` depends on `lanekeep-engine`, which depends on
//! `lanekeep-types` — so a dev-dependency from `lanekeep-types` on the testkit would close a
//! cycle in the publication order, which `scripts/publish-crates.sh` refuses outright
//! (`error: dependency cycle among …`) and which `crates/lanekeep-engine/Cargo.toml` already
//! declines to close for the same reason. `lanekeep-cli` is the top of the graph and already
//! dev-depends on the testkit.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The fixture helpers below, and the `corpus` \
              helpers this file also compiles, are neither."
)]
#![expect(
    clippy::print_stderr,
    reason = "a test that finds `typescript` absent has to say so on the terminal: the \
              alternative is a suite reporting a pass for a test it did not run"
)]

use std::path::{Path, PathBuf};

use lanekeep_core::Violation;
use lanekeep_testkit::RuleTester;

mod corpus;

use corpus::tsc_available;

/// One-based lines in the subject, so an assertion names positions rather than counts.
///
/// Counted off the sources in [`Fixture::run`] and [`Fixture::run_plain`], whose first line is
/// the import.
const PLAIN_LINE: u32 = 2;
const GENERIC_LINE: u32 = 4;
const CONDITIONAL_LINE: u32 = 5;

#[derive(Clone, Copy)]
enum Provider {
    Builtin,
    Tsc,
}

fn typescript_package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/lanekeep/node_modules/typescript")
}

/// The `types` block a fixture config carries, as TypeScript source.
///
/// An **absolute** path to this repository's own `typescript`, because the fixture project is
/// a throwaway directory in the system temp with no `node_modules` of its own. Spelled with
/// forward slashes: `validate_specifier` and JSON escaping both have history with backslashes
/// on Windows, and Rust accepts either separator there, so `C:/Users/...` is still absolute
/// and carries nothing either gate refuses.
fn types_block(provider: Provider) -> String {
    match provider {
        Provider::Builtin => "types: { provider: 'builtin' },".to_owned(),
        Provider::Tsc => format!(
            "types: {{ provider: 'tsc', typescript: '{}' }},",
            typescript_package()
                .canonicalize()
                .expect("the authoring package is installed")
                .to_string_lossy()
                .replace('\\', "/")
        ),
    }
}

/// The fixture: a project rule declaring `requires: ['types']`, a local library, and a subject
/// exercising the two constructs the built-in oracle documents itself as not computing.
///
/// Three things about the rule were measured rather than assumed, on 2026-09-08 against
/// `typescript` 5.9.3, and each of them decides a line of it.
///
/// `Amount` is an **interface**, not `type Amount = number`: `typeToString` of a variable
/// annotated with an alias of a primitive is `number`, the alias erased, so on the alias
/// spelling `tsc` answers `number` for all four declarations and the rule reports nothing at
/// all under it. An interface is nominal and its name survives.
///
/// The rule asks about the **annotation**, `m.type`, and not the declared name: the built-in
/// oracle answers `undefined` for a `variable_declarator`'s `(identifier)` and answers the
/// annotation, which is why `no-restricted-types` reads `m.type ?? m.name` and why
/// `AGENTS.md`'s type-parameter entry says every fixture there is anchored on a
/// `type_annotation`. Asking about the name makes both providers silent, which would let this
/// suite pass while comparing two rows of nothing.
///
/// And it matches on `text.includes('Amount')` rather than on equality, because `tsc` names
/// the type it was asked about rather than reducing it: `Box<Amount>['value']` comes back as
/// `Box<Amount>` and `Unwrap<Promise<Amount>>` as itself, not as `Amount`. Equality would make
/// the two providers agree — both silent — for reasons that have nothing to do with either
/// answering.
fn typed_fixture(name: &str, provider: Provider) -> Fixture {
    let rule = r"
        import { defineRule } from 'lanekeep';
        export default defineRule({
          id: 'local/typed',
          severity: 'error',
          requires: ['types'],
          card: {
            message: 'this expression resolves to Amount',
            remediation: 'use a plain number',
            examples: { bad: 'const a: Amount = x', good: 'const a: number = x' },
          },
          query: '(variable_declarator name: (identifier) @name type: (type_annotation) @type)',
          check(ctx, m) {
            const type = ctx.types.typeOf(m.type);
            if (type !== undefined && type.text.includes('Amount')) ctx.report(m.name);
          },
        });
    ";

    let tester = RuleTester::new(name, rule)
        .expect("writes the fixture project")
        .with_config_extra(&types_block(provider))
        .expect("rewrites the config with a types block");

    tester
        .write_fixture(
            "subject/lib.ts",
            "export interface Amount { readonly cents: number }\n\
             export type Unwrap<T> = T extends Promise<infer U> ? U : T\n\
             export type Box<T> = { value: T }\n",
        )
        .expect("writes the library");

    Fixture { tester }
}

struct Fixture {
    tester: RuleTester,
}

impl Fixture {
    /// The subject with no divergent construct in it: `plain: Amount` reports, `other: string`
    /// does not, and both providers agree. What the agreement test runs.
    fn run_plain(&self) -> Vec<Violation> {
        self.tester
            .run(
                "import type { Amount } from './lib'\n\
                 declare const plain: Amount\n\
                 declare const other: string\n\
                 export { plain, other }\n",
            )
            .expect("the run completes")
    }

    /// Check the subject with the two divergent constructs and hand back its violations,
    /// sorted as the engine sorts them. What the divergence test runs.
    fn run(&self) -> Vec<Violation> {
        self.tester
            .run(
                "import type { Amount, Unwrap, Box } from './lib'\n\
                 declare const plain: Amount\n\
                 declare const other: string\n\
                 declare const boxed: Box<Amount>['value']\n\
                 declare const unwrapped: Unwrap<Promise<Amount>>\n\
                 export { plain, other, boxed, unwrapped }\n",
            )
            .expect("the run completes")
    }
}

/// The lines a run reported at, which is what every assertion here compares.
fn lines(violations: &[Violation]) -> Vec<u32> {
    violations
        .iter()
        .map(|v| v.location.position.line)
        .collect()
}

#[test]
fn both_providers_agree_on_the_cross_file_suite() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    // The other plan's own acceptance, re-run under the other provider. Agreement is the
    // property that makes `types.provider` a choice rather than two products: a rule's verdict
    // may not depend on which oracle answered, except where the divergence test below
    // enumerates a divergence.
    let builtin = typed_fixture("agree-builtin", Provider::Builtin).run_plain();
    let tsc = typed_fixture("agree-tsc", Provider::Tsc).run_plain();
    assert_eq!(lines(&builtin), lines(&tsc));
    assert_eq!(
        lines(&builtin),
        vec![PLAIN_LINE],
        "the control: `plain: Amount` reports under both and `other: string` does not: \
         {builtin:?}"
    );
}

#[test]
fn the_enumerated_divergences_are_the_only_ones() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    // The divergences in *one* test, named, rather than scattered — so that the list is a
    // thing someone can read and disagree with. Two constructs the built-in oracle documents
    // itself as not computing: an indexed access into a generic instantiation, and a
    // conditional type. `tsc` resolves both, and the agreement above is otherwise exact — the
    // line the two lists share is the ordinary annotation.
    let builtin = typed_fixture("diverge-builtin", Provider::Builtin).run();
    let tsc = typed_fixture("diverge-tsc", Provider::Tsc).run();
    assert_eq!(
        lines(&builtin),
        vec![PLAIN_LINE],
        "the built-in oracle answers nothing for an indexed access into a generic \
         instantiation nor for a conditional type, so the rule reports only the plain \
         annotation: {builtin:?}"
    );
    assert_eq!(
        lines(&tsc),
        vec![PLAIN_LINE, GENERIC_LINE, CONDITIONAL_LINE],
        "and `tsc` answers for both — `Box<Amount>` and `Unwrap<Promise<Amount>>`, the types \
         as written rather than reduced, which is still an answer where the built-in oracle \
         has none: {tsc:?}"
    );
}

#[test]
fn two_runs_under_tsc_are_byte_identical() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    // The invariant, asserted for the provider that spawns a process — which is where it is
    // least obvious, because nothing about a process is deterministic by construction. Two
    // separate fixtures rather than one run twice, so a memo inside one provider cannot be
    // what makes the second answer match the first.
    let first = typed_fixture("stable-1", Provider::Tsc).run();
    let second = typed_fixture("stable-2", Provider::Tsc).run();
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
    // And that what was reported twice is the `tsc` answer rather than an empty list. Two runs
    // that both found nothing are byte-identical too, so equality on its own is a test a
    // silent provider passes.
    assert_eq!(
        lines(&first),
        vec![PLAIN_LINE, GENERIC_LINE, CONDITIONAL_LINE],
        "the sidecar answered: {first:?}"
    );
}
