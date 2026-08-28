//! `lanekeep/no-restricted-calls` over a mixed-language corpus, through the real binary.
//!
//! The per-file behavior lives in `lanekeep-rules/tests/no_restricted_calls.rs`; what only
//! a corpus can show is one configuration reaching all four grammars in one run, reported
//! in one deterministic order.

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
fn one_restriction_list_reaches_every_grammar_in_one_run() {
    let corpus = Corpus::new(
        "no-restricted-calls",
        "{ restrictions: [{ call: 'console.*' }, { call: 'open' }, { call: 'fmt.*' }, \
         { call: 'std::fs::*' }] }",
        &[
            ("src/a.ts", "console.log('x');\n"),
            ("src/b.py", "open('x')\n"),
            (
                "src/c.go",
                "package main\n\nfunc run() {\n\tfmt.Println(\"x\")\n}\n",
            ),
            ("src/d.rs", "fn run() {\n    std::fs::read(\"x\");\n}\n"),
        ],
    );

    let first = corpus.run();
    assert_eq!(
        first,
        vec![
            "src/a.ts:1:1 calling 'console.log' is restricted — this call is not allowed here",
            "src/b.py:1:1 calling 'open' is restricted — this call is not allowed here",
            "src/c.go:4:2 calling 'fmt.Println' is restricted — this call is not allowed here",
            "src/d.rs:2:5 calling 'std::fs::read' is restricted — this call is not allowed here",
        ]
    );

    for attempt in 0..3 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
}
