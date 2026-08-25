//! What a user is shown when a built-in rule throws, through the binary.
//!
//! The four TypeScript built-ins run as QuickJS modules, so a thrown error's stack names the
//! rule's module specifier — `lanekeep/no-restricted-imports` — with line and column that point
//! at the author's TypeScript (the stripper preserves offsets, so a position in the generated
//! JavaScript is the same position in the source). This is the native path: no source map to
//! wire, no bundle to remap, and the test asserts that the rule's own specifier appears in
//! the reported failure rather than an opaque `eval_script` frame.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

mod corpus;

use corpus::Corpus;

#[test]
fn a_built_in_that_throws_is_reported_in_the_typescript_it_was_authored_in() {
    // `restrictions: [{}]` is a restriction with no `module`, which sends the rule into
    // `matches(undefined, specifier)` and `undefined.split`. The rule's own source is frozen, so
    // the failure is provoked through its options.
    let reported = Corpus::new(
        "no-restricted-imports",
        "{ restrictions: [{}] }",
        &[(
            "src/a.ts",
            "import x from 'lodash/merge'\nexport const a = x\n",
        )],
    )
    .run_failing();

    assert!(
        reported.contains("lanekeep/no-restricted-imports:"),
        "the failure does not name the rule's own specifier:\n{reported}"
    );
}

#[test]
fn a_cross_file_rule_that_throws_is_reported_in_its_own_source_too() {
    // The second of the four, and it reaches a **different phase** through a different source
    // file: `entryPoints: 5` survives `configure` and dies at `entryPoints.includes` inside
    // `reduce`, which runs once for the whole corpus rather than per match.
    //
    // Worth having beside the test above rather than folded into it: `reduce`'s frames arrive
    // through a different path in the runtime than `check`'s, which nothing else here exercises.
    let reported = Corpus::new(
        "no-unused-exports",
        "{ entryPoints: 5 }",
        &[("src/a.ts", "export const a = 1\n")],
    )
    .run_failing();

    assert!(
        reported.contains("lanekeep/no-unused-exports:"),
        "the failure does not name the rule's own specifier:\n{reported}"
    );
}
