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

use lanekeep_core::{Severity, Violation};
use lanekeep_testkit::{RuleTester, TestError};

/// The rule id every `no-broad-except` violation carries.
const BROAD_EXCEPT_RULE_ID: &str = "lanekeep/no-broad-except";
/// The remediation every `no-broad-except` violation carries, from the rule's card.
const BROAD_EXCEPT_REMEDIATION: &str = "name the exceptions this block can actually raise, \
                                        so an unexpected one still surfaces";
/// The rule id every `no-mutable-default-argument` violation carries.
const MUTABLE_DEFAULT_RULE_ID: &str = "lanekeep/no-mutable-default-argument";
/// The remediation every `no-mutable-default-argument` violation carries, from the rule's card.
const MUTABLE_DEFAULT_REMEDIATION: &str = "default to None and build the value inside the \
                                           function, so each call gets its own";
/// The severity every violation carries, resolved by config rather than declared by the rule.
const SEVERITY: Severity = Severity::Error;

/// Hold every violation to the three fields the position and message assertions do not pin.
///
/// The fidelity spec says "identical positions, messages, ids, severities and remediations";
/// the `reports_at`/`reports_messages` helpers cover the first two, and this covers the last
/// three. Modeled on `no_context_in_struct.rs`'s `assert_identity`, which is where the argument
/// for checking all three is made: a component answering a mistyped id, a different severity or
/// a rewritten remediation passes every position and message case, and a migration whose whole
/// claim is that the two implementations report identically has to hold those too.
fn assert_identity(
    violations: &[Violation],
    rule_id: &str,
    remediation: &str,
) -> Result<(), TestError> {
    for violation in violations {
        let id = violation.rule_id.to_string();
        if id != rule_id {
            return Err(TestError::Mismatch(format!(
                "expected rule id `{rule_id}`, got `{id}`"
            )));
        }
        if violation.severity != SEVERITY {
            return Err(TestError::Mismatch(format!(
                "expected severity {SEVERITY:?}, got {:?}",
                violation.severity
            )));
        }
        if violation.remediation != remediation {
            return Err(TestError::Mismatch(format!(
                "expected remediation `{remediation}`, got `{}`",
                violation.remediation
            )));
        }
    }
    Ok(())
}

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
    let Some(tester) = tester("no-broad-except") else {
        return;
    };
    tester
        .accepts("try:\n    parse(raw)\nexcept ValueError:\n    return None\n")
        .expect("naming what the block can raise is the point");
    tester
        .accepts("try:\n    go()\nexcept (ValueError, KeyError):\n    pass\n")
        .expect("a tuple of named exceptions is still specific");
    tester
        .reports_at("try:\n    go()\nexcept:\n    pass\n", &[(3, 1)])
        .expect("a bare except also swallows Ctrl-C");
    assert_identity(
        &tester
            .run("try:\n    go()\nexcept:\n    pass\n")
            .expect("runs"),
        BROAD_EXCEPT_RULE_ID,
        BROAD_EXCEPT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at("try:\n    go()\nexcept Exception:\n    pass\n", &[(3, 1)])
        .expect("Exception covers every bug in the block");
    assert_identity(
        &tester
            .run("try:\n    go()\nexcept Exception:\n    pass\n")
            .expect("runs"),
        BROAD_EXCEPT_RULE_ID,
        BROAD_EXCEPT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at(
            "try:\n    go()\nexcept BaseException:\n    pass\n",
            &[(3, 1)],
        )
        .expect("BaseException is broader still");
    assert_identity(
        &tester
            .run("try:\n    go()\nexcept BaseException:\n    pass\n")
            .expect("runs"),
        BROAD_EXCEPT_RULE_ID,
        BROAD_EXCEPT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at(
            "try:\n    go()\nexcept Exception as err:\n    log(err)\n",
            &[(3, 1)],
        )
        .expect("binding the error does not narrow what is caught");
    assert_identity(
        &tester
            .run("try:\n    go()\nexcept Exception as err:\n    log(err)\n")
            .expect("runs"),
        BROAD_EXCEPT_RULE_ID,
        BROAD_EXCEPT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
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
    assert_identity(
        &tester
            .run("try:\n    go()\nexcept Exception:\n    pass\n")
            .expect("runs"),
        BROAD_EXCEPT_RULE_ID,
        BROAD_EXCEPT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
}

#[test]
fn the_python_no_mutable_default_argument_component_matches_the_typescript_original() {
    let Some(tester) = tester("no-mutable-default-argument") else {
        return;
    };
    tester
        .accepts("def add(item, items=None):\n    pass\n")
        .expect("None is immutable");
    tester
        .accepts("def f(a=1, b='x', c=(), d=False, e=None):\n    pass\n")
        .expect("immutable defaults pass");
    tester
        .reports_at("def add(item, items=[]):\n    pass\n", &[(1, 15)])
        .expect("a list default is created once");
    assert_identity(
        &tester
            .run("def add(item, items=[]):\n    pass\n")
            .expect("runs"),
        MUTABLE_DEFAULT_RULE_ID,
        MUTABLE_DEFAULT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at("def f(opts={}):\n    pass\n", &[(1, 7)])
        .expect("a dict default is created once");
    assert_identity(
        &tester.run("def f(opts={}):\n    pass\n").expect("runs"),
        MUTABLE_DEFAULT_RULE_ID,
        MUTABLE_DEFAULT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at("def f(seen={1}):\n    pass\n", &[(1, 7)])
        .expect("a set default is created once");
    assert_identity(
        &tester.run("def f(seen={1}):\n    pass\n").expect("runs"),
        MUTABLE_DEFAULT_RULE_ID,
        MUTABLE_DEFAULT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    for (source, at) in [
        ("def f(items=list()):\n    pass\n", (1, 7)),
        ("def f(opts=dict()):\n    pass\n", (1, 7)),
        ("def f(seen=set()):\n    pass\n", (1, 7)),
    ] {
        tester
            .reports_at(source, &[at])
            .unwrap_or_else(|e| panic!("{source} should be reported: {e}"));
        assert_identity(
            &tester.run(source).expect("runs"),
            MUTABLE_DEFAULT_RULE_ID,
            MUTABLE_DEFAULT_REMEDIATION,
        )
        .unwrap_or_else(|e| panic!("{source} identity should match: {e}"));
    }
    tester
        .reports_at("def f(a=[], b=1, c={}):\n    pass\n", &[(1, 7), (1, 18)])
        .expect("each one is its own shared object");
    assert_identity(
        &tester
            .run("def f(a=[], b=1, c={}):\n    pass\n")
            .expect("runs"),
        MUTABLE_DEFAULT_RULE_ID,
        MUTABLE_DEFAULT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
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
    assert_identity(
        &tester
            .run("def add(item, items=[]):\n    pass\n")
            .expect("runs"),
        MUTABLE_DEFAULT_RULE_ID,
        MUTABLE_DEFAULT_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
}
