//! `lanekeep/no-mutable-default-argument`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-mutable-default-argument").expect("the rule ships");
    RuleTester::with_extension("no-mutable-default-argument", source, "py").expect("builds")
}

#[test]
fn an_immutable_default_passes() {
    tester()
        .accepts("def add(item, items=None):\n    pass\n")
        .expect("None is the sanctioned form");
}

#[test]
fn other_immutable_defaults_pass() {
    tester()
        .accepts("def f(a=1, b='x', c=(), d=False, e=None):\n    pass\n")
        .expect("numbers, strings, tuples and booleans are all shared safely");
}

#[test]
fn a_list_default_is_reported() {
    tester()
        .reports_at("def add(item, items=[]):\n    pass\n", &[(1, 15)])
        .expect("the list is built once, at definition");
}

#[test]
fn a_dict_default_is_reported() {
    tester()
        .reports_at("def f(opts={}):\n    pass\n", &[(1, 7)])
        .expect("so is the dict");
}

#[test]
fn a_set_default_is_reported() {
    tester()
        .reports_at("def f(seen={1}):\n    pass\n", &[(1, 7)])
        .expect("and the set");
}

#[test]
fn the_constructor_spellings_are_reported() {
    for (source, at) in [
        ("def f(items=list()):\n    pass\n", (1, 7)),
        ("def f(opts=dict()):\n    pass\n", (1, 7)),
        ("def f(seen=set()):\n    pass\n", (1, 7)),
    ] {
        tester()
            .reports_at(source, &[at])
            .unwrap_or_else(|e| panic!("{source} should be reported: {e}"));
    }
}

#[test]
fn every_offending_parameter_is_reported() {
    tester()
        .reports_at("def f(a=[], b=1, c={}):\n    pass\n", &[(1, 7), (1, 18)])
        .expect("each one is its own shared object");
}

#[test]
fn a_project_defined_list_passes() {
    // Same reason `no-broad-except` uses the resolver: `list()` is only the builtin when
    // nothing has taken the name.
    tester()
        .accepts("def list():\n    return 1\n\ndef f(items=list()):\n    pass\n")
        .expect("a local `list` is not the builtin constructor");
}

#[test]
fn a_lambda_default_is_not_a_container() {
    tester()
        .accepts("def f(key=lambda x: x):\n    pass\n")
        .expect("a lambda is created once too, and sharing it is harmless");
}
