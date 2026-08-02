//! `lanekeep/no-context-in-struct`, run through the real engine.
//!
//! The first built-in rule for Go, so these check the whole path as much as the rule: a `.go`
//! file reaching the Go grammar, and the Go binding resolver answering `ctx.bindingKind` from
//! inside the sandbox.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-context-in-struct").expect("the rule ships");
    RuleTester::with_extension("no-context-in-struct", source, "go").expect("builds")
}

#[test]
fn a_context_passed_as_a_parameter_passes() {
    // The shape the rule is steering towards.
    tester()
        .accepts(
            "package a\n\nimport \"context\"\n\ntype Client struct{}\n\n\
             func (c *Client) Do(ctx context.Context) error { return nil }\n",
        )
        .expect("a parameter is scoped to the call, which is the whole point");
}

#[test]
fn a_struct_with_ordinary_fields_passes() {
    tester()
        .accepts("package a\n\nimport \"context\"\n\ntype Client struct {\n\turl string\n}\n")
        .expect("nothing here stores a context");
}

#[test]
fn a_stored_context_is_reported() {
    tester()
        .reports_at(
            "package a\n\nimport \"context\"\n\ntype Client struct {\n\tctx context.Context\n}\n",
            &[(6, 2)],
        )
        .expect("a stored context outlives the call it was scoped to");
}

#[test]
fn a_stored_pointer_to_context_is_reported() {
    // The pointer form needs its own query pattern, so it is the one most likely to be
    // silently missed.
    tester()
        .reports_at(
            "package a\n\nimport \"context\"\n\ntype Client struct {\n\tctx *context.Context\n}\n",
            &[(6, 2)],
        )
        .expect("a pointer to a context stores it just the same");
}

#[test]
fn an_aliased_context_import_is_reported() {
    // The alias is what a text match would miss.
    tester()
        .reports_at(
            "package a\n\nimport context \"context\"\n\ntype C struct {\n\tc context.Context\n}\n",
            &[(6, 2)],
        )
        .expect("aliasing the import to its own name changes nothing");
}

#[test]
fn a_different_package_aliased_to_context_is_also_reported() {
    // The rule's one known false positive, asserted rather than left to be discovered.
    //
    // `ctx.bindingKind` says a name is an import; it does not say which module. Telling
    // these apart needs the host API to expose the module, which bumps its version and so
    // the cache key — a change this rule does not justify on its own. Pinned here so that if
    // the API ever does grow that, this test fails and points at the rule to tighten.
    tester()
        .reports_at(
            "package a\n\nimport context \"example.com/app/context\"\n\n\
             type C struct {\n\tc context.Context\n}\n",
            &[(6, 2)],
        )
        .expect("indistinguishable from the standard library's context without module info");
}

#[test]
fn a_type_named_context_from_no_import_passes() {
    // No import at all: `context` here is a local package-level name, so the qualifier does
    // not resolve to an import and the rule must not fire.
    tester()
        .accepts("package a\n\ntype C struct {\n\tc context.Context\n}\n")
        .expect("an unresolved qualifier is not the standard library");
}

#[test]
fn a_similarly_named_field_type_passes() {
    tester()
        .accepts("package a\n\nimport \"context\"\n\ntype C struct {\n\tc context.CancelFunc\n}\n")
        .expect("only Context is the problem");
}
