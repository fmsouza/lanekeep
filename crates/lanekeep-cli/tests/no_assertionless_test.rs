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
