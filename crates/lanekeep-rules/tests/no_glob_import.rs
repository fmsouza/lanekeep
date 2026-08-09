//! `lanekeep/no-glob-import`, run through the real engine.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

fn tester() -> RuleTester {
    let source = lanekeep_rules::source("no-glob-import").expect("the rule ships");
    RuleTester::with_extension("no-glob-import", source, "rs").expect("builds")
}

/// A tester for the rule as its own documentation spells it: `noGlobImport({ allow: [...] })`.
fn configured(options: &str) -> RuleTester {
    let source = lanekeep_rules::source("no-glob-import").expect("the rule ships");
    RuleTester::configured_with_extension("no-glob-import", source, "rs", options).expect("builds")
}

#[test]
fn a_named_use_passes() {
    tester()
        .accepts("use std::collections::HashMap;\n")
        .expect("naming what you import is the point");
}

#[test]
fn a_use_list_passes() {
    tester()
        .accepts("use std::io::{Read, Write};\n")
        .expect("a list still says where each name comes from");
}

#[test]
fn a_glob_is_reported() {
    tester()
        .reports_at("use crate::models::*;\n", &[(1, 1)])
        .expect("a glob hides where every name comes from");
}

#[test]
fn a_prelude_glob_passes_by_default() {
    // The one shape a glob is the intended spelling of. A project using one should not have
    // to suppress this on every file.
    tester()
        .accepts("use std::prelude::v1::*;\nuse crate::prelude::*;\n")
        .expect("preludes are allowed by default");
}

#[test]
fn an_allowed_pattern_passes() {
    // The option the rule's own JSDoc documents, which reached the handler as `undefined` on
    // every run because the default export was a plain object rather than a factory.
    configured("{ allow: ['crate::internal::*'] }")
        .accepts("use crate::internal::*;\n")
        .expect("a configured pattern exempts the glob");
}

#[test]
fn a_configured_allow_list_replaces_the_prelude_default() {
    // `allow` is a replacement, not an addition — the default is a fallback for a rule that
    // was given nothing. Asserted because the opposite reading is just as plausible from the
    // source, and silently keeping the default would make the list above look wider than it is.
    configured("{ allow: ['crate::internal::*'] }")
        .reports_at("use crate::prelude::*;\n", &[(1, 1)])
        .expect("an explicit list replaces the default rather than extending it");
}

#[test]
fn a_prefix_alone_does_not_match_a_glob_path() {
    // `super` must not match `super::*`: the pattern is anchored at both ends, so a bare
    // prefix is not a match. Load-bearing history — the anchoring is what makes `allow`
    // narrow enough to be safe.
    configured("{ allow: ['super'] }")
        .reports_at("use super::*;\n", &[(1, 1)])
        .expect("an allow pattern is anchored at both ends");
}

#[test]
fn the_message_names_the_glob_once() {
    // tree-sitter-rust's `use_wildcard` is `(path '::')? '*'`, so the captured text already
    // ends in `::*`. Appending another produced `use crate::models::*::*` in every message
    // this rule has ever reported. Nothing caught it: no test asserted a message, and the
    // default `*prelude*` pattern matches either spelling.
    tester()
        .reports_messages(
            "use crate::models::*;\n",
            &["`use crate::models::*` hides where every name in this file comes from"],
        )
        .expect("the wildcard text already carries its own `::*`");
}

#[test]
fn every_glob_in_a_file_is_reported() {
    tester()
        .reports_at("use a::*;\nuse b::*;\n", &[(1, 1), (2, 1)])
        .expect("two globs are two problems");
}
