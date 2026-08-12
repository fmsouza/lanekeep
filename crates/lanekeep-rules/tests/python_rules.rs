//! The Python-authored rules, run through the real engine.
//!
//! `lanekeep/no-broad-except` and `lanekeep/no-mutable-default-argument` are authored in
//! Python in `py-rules/`, compiled by `componentize-py --stub-wasi` into one component,
//! and exercised here through the real engine. The artifact is not committed — the build
//! is not byte-reproducible — so `just py-rules` builds it and sets
//! `LANEKEEP_PY_RULES_WASM`; without it, these tests skip.
//!
//! The cases are the TypeScript originals' own, held to the same expectations. The
//! component hosts both rules, and a case for one rule is a case the other reports
//! nothing on — `no-broad-except` is gated on `fileContains: ['except']`, and the
//! `no-mutable-default-argument` fixtures contain no `except` — so the assertions read
//! the full violation list without filtering.

#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use lanekeep_testkit::RuleTester;

/// The artifact the recipe built, or None when it is absent (the tests skip).
fn component_bytes() -> Option<Vec<u8>> {
    let path = std::env::var("LANEKEEP_PY_RULES_WASM").ok()?;
    std::fs::read(path).ok()
}

/// A tester over the Python component, or None when the artifact is absent.
fn tester(name: &str) -> Option<RuleTester> {
    let bytes = component_bytes()?;
    Some(RuleTester::for_component(name, &bytes, "py").expect("builds"))
}

#[test]
fn the_python_no_broad_except_component_matches_the_typescript_original() {
    let Some(tester) = tester("no-broad-except") else { return };
    tester
        .accepts("try:\n    parse(raw)\nexcept ValueError:\n    return None\n")
        .expect("naming what the block can raise is the point");
    tester
        .accepts("try:\n    go()\nexcept (ValueError, KeyError):\n    pass\n")
        .expect("a tuple of named exceptions is still specific");
    tester
        .reports_at("try:\n    go()\nexcept:\n    pass\n", &[(3, 1)])
        .expect("a bare except also swallows Ctrl-C");
    tester
        .reports_at("try:\n    go()\nexcept Exception:\n    pass\n", &[(3, 1)])
        .expect("Exception covers every bug in the block");
    tester
        .reports_at("try:\n    go()\nexcept BaseException:\n    pass\n", &[(3, 1)])
        .expect("BaseException is broader still");
    tester
        .reports_at(
            "try:\n    go()\nexcept Exception as err:\n    log(err)\n",
            &[(3, 1)],
        )
        .expect("binding the error does not narrow what is caught");
    tester
        .accepts("class Exception:\n    pass\n\ntry:\n    go()\nexcept Exception:\n    pass\n")
        .expect("a local class shadows the builtin");
    tester
        .accepts("from .errors import Exception\n\ntry:\n    go()\nexcept Exception:\n    pass\n")
        .expect("an imported name is not the builtin either");
    tester
        .reports_messages(
            "try:\n    go()\nexcept Exception:\n    pass\n",
            &["`except Exception` catches every error the block can raise, including bugs"],
        )
        .expect("the message is the TypeScript original's");
}

#[test]
fn the_python_no_mutable_default_argument_component_matches_the_typescript_original() {
    let Some(tester) = tester("no-mutable-default-argument") else { return };
    tester
        .accepts("def add(item, items=None):\n    pass\n")
        .expect("None is immutable");
    tester
        .accepts("def f(a=1, b='x', c=(), d=False, e=None):\n    pass\n")
        .expect("immutable defaults pass");
    tester
        .reports_at("def add(item, items=[]):\n    pass\n", &[(1, 15)])
        .expect("a list default is created once");
    tester
        .reports_at("def f(opts={}):\n    pass\n", &[(1, 7)])
        .expect("a dict default is created once");
    tester
        .reports_at("def f(seen={1}):\n    pass\n", &[(1, 7)])
        .expect("a set default is created once");
    for (source, at) in [
        ("def f(items=list()):\n    pass\n", (1, 7)),
        ("def f(opts=dict()):\n    pass\n", (1, 7)),
        ("def f(seen=set()):\n    pass\n", (1, 7)),
    ] {
        tester
            .reports_at(source, &[at])
            .unwrap_or_else(|e| panic!("{source} should be reported: {e}"));
    }
    tester
        .reports_at("def f(a=[], b=1, c={}):\n    pass\n", &[(1, 7), (1, 18)])
        .expect("each one is its own shared object");
    tester
        .accepts("def list():\n    return 1\n\ndef f(items=list()):\n    pass\n")
        .expect("a local `list` is not the builtin constructor");
    tester
        .accepts("def f(key=lambda x: x):\n    pass\n")
        .expect("a lambda is created once too, and sharing it is harmless");
    tester
        .reports_messages(
            "def add(item, items=[]):\n    pass\n",
            &["default `[]` is created once and shared by every call"],
        )
        .expect("the message is the TypeScript original's");
}
