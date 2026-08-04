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
    RuleTester::configured("no-glob-import", source, "{}").expect("builds")
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
fn every_glob_in_a_file_is_reported() {
    tester()
        .reports_at("use a::*;\nuse b::*;\n", &[(1, 1), (2, 1)])
        .expect("two globs are two problems");
}

#[test]
fn an_allowed_pattern_passes() {
    // The pattern has to match `ctx.text(m.wildcard)`'s actual span, which is the whole
    // `prefix::*` — not only the prefix. `'super'` alone never matches `"super::*"`.
    let source = lanekeep_rules::source("no-glob-import").expect("the rule ships");
    RuleTester::configured("no-glob-import-allow", source, "{ allow: ['super::*'] }")
        .expect("builds")
        .accepts("use super::*;\n")
        .expect("`allow` is documented and has to work");
}
