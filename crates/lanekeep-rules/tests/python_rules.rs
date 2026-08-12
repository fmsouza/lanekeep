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
/// The rule id every `py-explicit-encoding` violation carries.
const EXPLICIT_ENCODING_RULE_ID: &str = "local/py-explicit-encoding";
/// The remediation every `py-explicit-encoding` violation carries, from the rule's card.
const EXPLICIT_ENCODING_REMEDIATION: &str = "pass `encoding=\"utf-8\"` — the default is \
                                            locale-dependent, and on Windows it is cp1252";
/// The rule id every `py-stdout-buffer` violation carries.
const STDOUT_BUFFER_RULE_ID: &str = "local/py-stdout-buffer";
/// The remediation every `py-stdout-buffer` violation carries, from the rule's card.
const STDOUT_BUFFER_REMEDIATION: &str = "write bytes through `sys.stdout.buffer.write`, which \
                                         neither re-encodes nor translates newlines";
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

#[test]
fn the_python_py_explicit_encoding_component_matches_the_typescript_original() {
    let Some(tester) = tester("py-explicit-encoding") else {
        return;
    };
    tester
        .reports_at("def go(p):\n    return open(p)\n", &[(2, 12)])
        .expect("Windows defaults to cp1252 and the failure is a truncated read");
    assert_identity(
        &tester
            .run("def go(p):\n    return open(p)\n")
            .expect("runs"),
        EXPLICIT_ENCODING_RULE_ID,
        EXPLICIT_ENCODING_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .reports_at("def go(p):\n    return Path(p).read_text()\n", &[(2, 12)])
        .expect("read_text takes the same default");
    tester
        .reports_at(
            "def go(p, data):\n    return Path(p).write_text(data)\n",
            &[(2, 12)],
        )
        .expect("write_text takes the same default as read_text, and needs the same encoding");
    tester
        .accepts("def go(p):\n    a = open(p, encoding=\"utf-8\")\n    b = Path(p).read_text(encoding=\"utf-8\")\n")
        .expect("naming the encoding is the whole fix");
    tester
        .accepts("def go(p):\n    return len(p)\n")
        .expect("only the text-reading calls take an encoding");
    tester
        .accepts("def go(p):\n    return open(p, \"rb\")\n")
        .expect("a binary open takes no encoding at all; there is nothing to add");
    tester
        .accepts("def go(p):\n    return open(p, mode=\"rb\")\n")
        .expect("mode is still binary whether it is positional or a keyword");
    tester
        .reports_at("def go(p):\n    return open(p, \"r\")\n", &[(2, 12)])
        .expect("text mode still needs an explicit encoding; only binary is exempt");
    tester
        .reports_at("def go():\n    return open(\"b.txt\")\n", &[(2, 12)])
        .expect("a path containing `b` is not a mode; only the second positional argument is");
    tester
        .reports_at(
            "def go(p, readable_mode):\n    return open(p, mode=readable_mode)\n",
            &[(2, 12)],
        )
        .expect("a mode that is not a string literal cannot be proven binary, so it must still be reported");
    tester
        .accepts("def go(p):\n    return open(p, mode=\"rb\")\n")
        .expect(
            "a string-literal mode keyword is exactly what the new check is supposed to accept",
        );
    tester
        .reports_at("def go(p):\n    return open(p, buffering=1)\n", &[(2, 12)])
        .expect("buffering is not mode; only a keyword named mode can indicate binary");
    tester
        .reports_at(
            "def go(p):\n    return open(p, errors=\"backslashreplace\")\n",
            &[(2, 12)],
        )
        .expect(
            "errors is not mode, and its value being a string that contains `b` must not matter",
        );
    tester
        .reports_messages(
            "def go(p):\n    return open(p)\n",
            &["`open` without `encoding=` reads cp1252 on Windows, which fails on the first non-ASCII byte"],
        )
        .expect("the message is the TypeScript original's");
    assert_identity(
        &tester
            .run("def go(p):\n    return open(p)\n")
            .expect("runs"),
        EXPLICIT_ENCODING_RULE_ID,
        EXPLICIT_ENCODING_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
}

#[test]
fn the_python_py_stdout_buffer_component_matches_the_typescript_original() {
    let Some(tester) = tester("py-stdout-buffer") else {
        return;
    };
    tester
        .reports_at("import sys\nsys.stdout.write(\"hi\")\n", &[(2, 1)])
        .expect("stdout re-encodes to cp1252 on Windows and truncates");
    assert_identity(
        &tester
            .run("import sys\nsys.stdout.write(\"hi\")\n")
            .expect("runs"),
        STDOUT_BUFFER_RULE_ID,
        STDOUT_BUFFER_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
    tester
        .accepts("import sys\nsys.stdout.buffer.write(b\"hi\")\n")
        .expect("bytes are neither re-encoded nor newline-translated");
    tester
        .accepts("def go(f):\n    f.write(\"hi\")\n# sys.stdout\n")
        .expect("only sys.stdout has the encoding problem");
    tester
        .accepts("import sys\nsys.stdout.flush()\n")
        .expect("only write carries the encoding and newline problem; flush takes no text");
    tester
        .accepts("print(\"hi\")\n# sys.stdout\n")
        .expect("print shares the encoding problem but this rule's query does not reach it");
    tester
        .reports_messages(
            "import sys\nsys.stdout.write(\"hi\")\n",
            &["sys.stdout encodes with the locale codec, which on Windows truncates the output at the first non-ASCII byte"],
        )
        .expect("the message is the TypeScript original's");
    assert_identity(
        &tester
            .run("import sys\nsys.stdout.write(\"hi\")\n")
            .expect("runs"),
        STDOUT_BUFFER_RULE_ID,
        STDOUT_BUFFER_REMEDIATION,
    )
    .expect("the id, severity and remediation match the TypeScript original's");
}
