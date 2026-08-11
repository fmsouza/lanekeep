//! What a user is shown when a built-in rule throws, through the binary.
//!
//! `crates/lanekeep-rules/tests/source_maps.rs` asserts that the remapping works. This asserts
//! that it is *wired*, which is a different claim and the one that can be lost silently.
//!
//! A built-in's source map reaches the runtime through four crates — `lanekeep_rules`'
//! component table, `RuleRoot::with_builtin_component_maps`, `ComponentRule::source_map`,
//! `ComponentLoader::load_mapped` — and `main.rs` is where the first of those is installed. A
//! build that installed the component lookup and not the map lookup would run every rule
//! correctly, report every violation at the right position, and pass every other test in the
//! tree; the only symptom is that a thrown rule names `entry.js`. So the wiring is asserted from
//! outside the binary, where nothing can stand in for it.

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
        reported.contains("crates/lanekeep-rules/rules/no-restricted-imports.ts:"),
        "the failure does not name the rule's own source:\n{reported}"
    );
    // And not the bundle it was compiled into, nor the build machine's temporary directory,
    // which is where `componentize-js`'s own generated glue reports from.
    //
    // `@entry.js:` rather than `entry.js:`, and the difference is a real frame rather than
    // pedantry: the stack legitimately ends in `packages/lanekeep/runtime/entry.js`, which is
    // lanekeep's own runtime calling the rule and a file a reader can open. What must not be
    // there is the bundle, which a frame spells with no directory in front of it at all.
    assert!(!reported.contains("@entry.js:"), "{reported}");
    assert!(!reported.contains("initializer.js"), "{reported}");
}

#[test]
fn a_cross_file_rule_that_throws_is_reported_in_its_own_source_too() {
    // The second of the four, and it reaches a **different phase** through a different source
    // file: `entryPoints: 5` survives `configure` and dies at `entryPoints.includes` inside
    // `reduce`, which runs once for the whole corpus rather than per match.
    //
    // Worth having beside the test above rather than folded into it. The two rules are separate
    // programs at separate offsets in one bundle, so a map right about one and wrong about the
    // other passes either test alone — and `reduce`'s frames arrive through a different path in
    // the runtime than `check`'s, which nothing else here exercises.
    //
    // **This was reported as needing "a `reduce`-driving harness built for the purpose", and
    // that was wrong.** It is true of the in-process route in
    // `crates/lanekeep-rules/tests/source_maps.rs`, which would need facts and a file list to
    // build a `reduce-context`. It is not true here: the binary builds all of that from a corpus
    // on disk, which is what `Corpus` already writes.
    let reported = Corpus::new(
        "no-unused-exports",
        "{ entryPoints: 5 }",
        &[("src/a.ts", "export const a = 1\n")],
    )
    .run_failing();

    assert!(
        reported.contains("crates/lanekeep-rules/rules/no-unused-exports.ts:"),
        "the failure does not name the rule's own source:\n{reported}"
    );
    assert!(!reported.contains("@entry.js:"), "{reported}");
    assert!(!reported.contains("initializer.js"), "{reported}");
}
