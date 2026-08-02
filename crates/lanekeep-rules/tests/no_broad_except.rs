//! `lanekeep/no-broad-except`, run through the real engine.
//!
//! The first built-in rule for a language other than JavaScript, so these check the whole
//! path as much as the rule: a Python file reaching a Python grammar, and the Python binding
//! resolver answering `ctx.bindingKind` from inside the sandbox.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-broad-except").expect("the rule ships");
    RuleTester::with_extension("no-broad-except", source, "py").expect("builds")
}

#[test]
fn a_named_exception_passes() {
    tester()
        .accepts("try:\n    parse(raw)\nexcept ValueError:\n    return None\n")
        .expect("naming what the block can raise is the point");
}

#[test]
fn several_named_exceptions_pass() {
    tester()
        .accepts("try:\n    go()\nexcept (ValueError, KeyError):\n    pass\n")
        .expect("a tuple of named exceptions is still specific");
}

#[test]
fn a_bare_except_is_reported() {
    tester()
        .reports_at("try:\n    go()\nexcept:\n    pass\n", &[(3, 1)])
        .expect("a bare except also swallows Ctrl-C");
}

#[test]
fn catching_exception_is_reported() {
    tester()
        .reports_at("try:\n    go()\nexcept Exception:\n    pass\n", &[(3, 1)])
        .expect("Exception covers every bug in the block");
}

#[test]
fn catching_base_exception_is_reported() {
    tester()
        .reports_at(
            "try:\n    go()\nexcept BaseException:\n    pass\n",
            &[(3, 1)],
        )
        .expect("BaseException is broader still");
}

#[test]
fn the_as_form_is_reported_too() {
    // `except Exception as e` wraps the name in an `as_pattern`, so a rule reading the
    // clause's first child directly would miss it.
    tester()
        .reports_at(
            "try:\n    go()\nexcept Exception as err:\n    log(err)\n",
            &[(3, 1)],
        )
        .expect("binding the error does not narrow what is caught");
}

#[test]
fn a_project_defined_exception_of_the_same_name_passes() {
    // The reason this rule uses the resolver rather than matching text. A project with its
    // own `Exception` class is not catching the builtin, and reporting it would be wrong.
    tester()
        .accepts("class Exception:\n    pass\n\ntry:\n    go()\nexcept Exception:\n    pass\n")
        .expect("a local class shadows the builtin");
}

#[test]
fn an_imported_exception_of_the_same_name_passes() {
    tester()
        .accepts("from .errors import Exception\n\ntry:\n    go()\nexcept Exception:\n    pass\n")
        .expect("an imported name is not the builtin either");
}
