//! The published type definitions describe the host API this crate actually registers.
//!
//! A definition that drifts from the engine is worse than none. The two directions fail
//! differently and both are silent:
//!
//! - **Types claim something the host lacks.** An author gets confident autocomplete for a
//!   method that does not exist, writes a rule against it, and finds out when the rule throws
//!   at run time — inside a sandbox, from a stack trace pointing at their handler.
//! - **The host has something the types lack.** The method works but is invisible, so it goes
//!   unused or gets reached through a cast that silences every other check on that line.
//!
//! Both are checked here, against `host.rs` itself rather than against a list someone
//! maintains. A list would be a third thing to keep in step.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::collections::BTreeSet;

/// The host's own source. Read at compile time so the test cannot drift from the file it
/// describes by being pointed at a stale copy.
const HOST: &str = include_str!("../src/host.rs");

/// The published definitions.
const TYPES: &str = include_str!("../../../packages/lanekeep/index.d.ts");

/// Names registered on a context object, from `object.set("name", ...)`.
///
/// Only the half above `#[cfg(test)]`: the test module below it builds objects with the same
/// call, and its fixtures are not API.
fn registered() -> BTreeSet<String> {
    let body = HOST.split("#[cfg(test)]").next().unwrap_or(HOST);
    let mut names = BTreeSet::new();

    for (index, _) in body.match_indices("object.set(") {
        let rest = &body[index + "object.set(".len()..];
        // The name is the next quoted string, possibly after a newline and indentation.
        let Some(open) = rest.find('"') else { continue };
        // Guard against a `set(` whose first argument is not a literal: the quote has to be
        // within the same call, not somewhere later in the file.
        if rest[..open].contains(')') {
            continue;
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else {
            continue;
        };
        names.insert(after[..close].to_owned());
    }
    names
}

/// Members declared on the two context interfaces in the `.d.ts`.
fn declared() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for interface in [
        "export interface RuleContext {",
        "export interface ReduceContext {",
    ] {
        let start = TYPES
            .find(interface)
            .unwrap_or_else(|| panic!("`{interface}` is missing from the definitions"));
        let body = &TYPES[start + interface.len()..];
        let end = body.find("\n}").expect("the interface is closed");

        for line in body[..end].lines() {
            let line = line.trim();
            // Skip documentation, modifiers and blank lines; a member is `name(` or `name:`.
            if line.is_empty()
                || line.starts_with("//")
                || line.starts_with('*')
                || line.starts_with("/*")
            {
                continue;
            }
            let line = line.strip_prefix("readonly ").unwrap_or(line);
            let name: String = line
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let after = &line[name.len()..];
            if after.starts_with('(') || after.starts_with(':') || after.starts_with("?(") {
                names.insert(name);
            }
        }
    }
    names
}

/// Registered names that are not context members.
///
/// `report` builds a location object with the same `object.set` call, so these three are the
/// violation's fields rather than anything an author reaches. Named individually rather than
/// filtered by heuristic, so a genuinely new host function cannot hide among them.
const NOT_CONTEXT_MEMBERS: &[&str] = &["file", "loc"];

#[test]
fn the_types_claim_nothing_the_host_does_not_provide() {
    let registered = registered();
    let declared = declared();
    let invented: Vec<&String> = declared
        .iter()
        .filter(|name| !registered.contains(*name))
        .collect();

    assert!(
        invented.is_empty(),
        "packages/lanekeep/index.d.ts declares {invented:?}, which host.rs does not register. \
         An author would get autocomplete for a method that throws at run time."
    );
}

#[test]
fn the_types_cover_everything_the_host_provides() {
    let declared = declared();
    let registered = registered();
    let missing: Vec<&String> = registered
        .iter()
        .filter(|name| !declared.contains(*name))
        .filter(|name| !NOT_CONTEXT_MEMBERS.contains(&name.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "host.rs registers {missing:?}, which packages/lanekeep/index.d.ts does not declare. \
         The method works but is invisible to anyone writing a rule."
    );
}

#[test]
fn every_binding_kind_the_resolvers_can_return_is_typed() {
    // `ctx.bindingKind` is typed as a union rather than `string`, which is only useful while
    // the union is complete. A language crate adding a kind without adding it here would
    // narrow an author's `switch` to something wrong — silently, since the missing arm just
    // never matches.
    //
    // Read from `as_str`'s match arms rather than from a list, for the same reason the host
    // surface is read from `host.rs`: the compiler already forces that match to be
    // exhaustive, so it is the one place that cannot fall behind the enum.
    const KINDS: &str = include_str!("../../lanekeep-lang/src/binding.rs");

    let arms = KINDS
        .split("pub const fn as_str(self)")
        .nth(1)
        .expect("BindingKind::as_str is where the strings live");
    let arms = &arms[..arms.find("\n    }").expect("the function is closed")];

    let mut found = 0_usize;
    let mut missing = Vec::new();
    for line in arms.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Self::") else {
            continue;
        };
        let Some(start) = rest.find('"') else {
            continue;
        };
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { continue };
        let kind = &after[..end];
        found += 1;
        if !TYPES.contains(&format!("| '{kind}'")) {
            missing.push(kind.to_owned());
        }
    }

    // A parse that quietly matched nothing would make every assertion below vacuous, and the
    // test would go green forever while checking nothing. There are more than a dozen kinds;
    // any figure well above zero catches a broken extraction without pinning an exact count
    // that a new language would have to update.
    assert!(
        found >= 10,
        "only {found} binding kinds were extracted from as_str — the parse is broken, so this \
         test is asserting nothing"
    );
    assert!(
        missing.is_empty(),
        "BindingKind can return {missing:?}, which the union in index.d.ts does not include"
    );
}
