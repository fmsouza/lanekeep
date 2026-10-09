//! `lanekeep/no-assertionless-test` over a mixed-language corpus, through the real binary.
//!
//! The per-language behavior lives in `lanekeep-rules/tests/no_assertionless_test.rs`; what
//! only a corpus can show is one rule reaching all four grammars in one run, each offender
//! reported under the one id, in one deterministic order.

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
fn one_rule_reaches_every_grammar_in_one_run() {
    let corpus = Corpus::new(
        "no-assertionless-test",
        "{}",
        &[
            ("src/a.test.ts", "it('adds', () => {\n  add(1, 2)\n})\n"),
            ("src/b_test.py", "def test_add():\n    helper()\n"),
            (
                "src/c_test.go",
                "package main\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\thelper()\n}\n",
            ),
            (
                "src/d.rs",
                "#[test]\nfn adds() {\n    helper();\n}\n\n#[test]\nfn checks() {\n    assert!(works());\n}\n",
            ),
        ],
    );

    let first = corpus.run();
    assert_eq!(
        first,
        vec![
            "src/a.test.ts:1:1 test asserts nothing",
            "src/b_test.py:1:1 test 'test_add' asserts nothing",
            "src/c_test.go:5:1 test 'TestAdd' asserts nothing",
            "src/d.rs:2:1 test 'adds' asserts nothing",
        ]
    );

    for attempt in 0..3 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
}

#[test]
fn aliases_are_judged_per_file_across_a_whole_run() {
    // #292. Which framework modules a file quotes is remembered for the last text asked
    // about, in module state that one worker's sandbox carries from file to file. A hook file
    // asks and quotes none; the alias file after it must not be answered from that memo. The
    // hook files are the ones that ask: `it` and a `testCallees` name are known without the
    // question, and `useEffect` is not. The names interleave the three kinds in check order,
    // and only one worker carries state across files — under the default pool, `map_init`
    // gives each file of a corpus this small its own sandbox and a stale memo goes unseen.
    // A `testCallees` name rides along, through `lanekeep.json` rather than a `.config.ts`.
    let mut owned = Vec::new();
    let mut expected = Vec::new();
    for index in 0..8 {
        owned.push((
            format!("src/{index}a-hook.ts"),
            "import { useEffect } from 'react'\nuseEffect(() => {\n  run()\n}, [])\n".to_owned(),
        ));
        owned.push((
            format!("src/{index}b-alias.test.ts"),
            "import { it as base } from 'vitest'\nbase('x', () => {\n  void 1\n})\n".to_owned(),
        ));
        owned.push((
            format!("src/{index}c-named.test.ts"),
            "import { test as pw } from './fixtures'\npw('x', () => {\n  void 2\n})\n".to_owned(),
        ));
        expected.push(format!(
            "src/{index}b-alias.test.ts:2:1 test asserts nothing"
        ));
        expected.push(format!(
            "src/{index}c-named.test.ts:2:1 test asserts nothing"
        ));
    }
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let corpus = Corpus::new("no-assertionless-test", "{ testCallees: ['pw'] }", &files);

    assert_eq!(corpus.run_on_one_worker(), expected);
    assert_eq!(
        corpus.run(),
        expected,
        "how many workers share the memo must not change the output"
    );
}
