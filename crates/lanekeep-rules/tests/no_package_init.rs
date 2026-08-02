//! `lanekeep/no-package-init`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-package-init").expect("the rule ships");
    RuleTester::with_extension("no-package-init", source, "go").expect("builds")
}

#[test]
fn an_ordinary_function_passes() {
    tester()
        .accepts("package a\n\nfunc Setup() error { return nil }\n")
        .expect("something main can call in an order it chooses");
}

#[test]
fn a_package_init_is_reported() {
    tester()
        .reports_at("package a\n\nfunc init() {\n\tregister()\n}\n", &[(3, 1)])
        .expect("init runs at import time in an order nothing states");
}

#[test]
fn every_init_in_a_file_is_reported() {
    // Go permits several, which is exactly what makes the ordering hard to reason about.
    tester()
        .reports_at(
            "package a\n\nfunc init() {\n\ta()\n}\n\nfunc init() {\n\tb()\n}\n",
            &[(3, 1), (7, 1)],
        )
        .expect("two inits in one file are two problems");
}

#[test]
fn a_local_variable_named_init_passes() {
    // The rule matches a declaration, not the text `init`, so a variable of that name is
    // untouched — and the `fileContains` gate deliberately admits this file.
    tester()
        .accepts("package a\n\nfunc f() {\n\tinit := 1\n\t_ = init\n}\n")
        .expect("a variable is not a package initializer");
}

#[test]
fn a_method_named_init_passes() {
    // A method is a `method_declaration`, which the query cannot match. It is also not
    // called implicitly, so it does not have the property the rule objects to.
    tester()
        .accepts("package a\n\ntype T struct{}\n\nfunc (t *T) init() {}\n")
        .expect("a method named init is called explicitly like any other");
}

#[test]
fn a_function_literal_named_by_a_variable_passes() {
    tester()
        .accepts("package a\n\nfunc f() {\n\tinit := func() {}\n\tinit()\n}\n")
        .expect("a func literal assigned to a variable runs when called");
}
