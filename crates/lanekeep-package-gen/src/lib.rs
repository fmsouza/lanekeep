//! Renders the built-in subpath mapping in `packages/lanekeep/package.json` and the
//! `packages/lanekeep/types.test-d.ts` gate from `COMPONENT_RULES`.
//!
//! `package.json`'s `exports` and `typesVersions` cannot tell a component built-in from a
//! module built-in when they are a single `"./*"` catch-all: every specifier resolves to
//! `builtin.d.ts`, which declares a default export, so `import x from 'lanekeep/no-unwrap'`
//! type-checks and then fails at run time because a component has no module to import. This
//! crate replaces that catch-all with a per-name mapping generated from the authoritative
//! table in `crates/lanekeep-rules`:
//!
//! - a **component** built-in (`is_declared_component`) is omitted from both fields, so
//!   importing it is a `Cannot find module` resolution error rather than a default export
//!   that lies;
//! - a **module** built-in (the rest of `names()`) points at `builtin.d.ts`, whose default
//!   `Rule & ((options?) => Rule)` is the honest type for an importable built-in.
//!
//! A migration that moves a rule between the two tables updates both fields automatically the
//! next time this runs, and `tests/generated.rs` fails the gate if the committed files are
//! not the ones this renders — the hand-maintained list is the thing that went stale before,
//! and this is the half of the trade that keeps it from going stale again.
//!
//! # Text surgery, not a JSON round-trip
//!
//! Only the `exports` and `typesVersions` values are rewritten; every other top-level field
//! (`name`, `private`, `types`, `devDependencies`, …) is left byte-for-byte as the committed
//! file holds it. A `serde_json` round-trip is deliberately not used: it would reorder keys
//! (a `BTreeMap` sorts them) unless `preserve_order` is enabled, and that feature is
//! workspace-wide — enabling it here would flip `lanekeep-config`'s `serde_json::Map` from a
//! `BTreeMap` to an `IndexMap` and silently break its config-hash invariants. Brace-matching
//! the two managed values keeps the rest of the file untouched and the diff to the managed
//! fields only.

// A generator over a small, known-shape JSON file: a missing `exports` or `typesVersions` is
// a programmer error, and panicking with a message that names it is the actionable failure.
// The workspace `[lints]` forbid these, so they are relaxed here, not silenced.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a malformed package.json is a programmer error; the panic names it"
)]

use std::fmt::Write as _;

/// The types target a module built-in points at — the file that declares the default export
/// an importable built-in needs.
const BUILTIN_DTS: &str = "builtin.d.ts";

/// Render the full `packages/lanekeep/package.json` text from the committed text, rewriting
/// only `exports` and `typesVersions` from `COMPONENT_RULES`.
///
/// `current` is the complete committed `package.json`. The returned string is the complete
/// file with the two managed values replaced; every other field is preserved byte-for-byte.
///
/// # Panics
///
/// Panics if `current` is not valid JSON or lacks `exports`/`typesVersions` — a hand edit that
/// broke `package.json` is a programmer error the panic names, on the same terms
/// `crates/lanekeep-types-gen` parses `world.wit`.
pub fn render_package_json(current: &str) -> String {
    // Normalize CRLF to LF so the preserved fields and the generated blocks share one line
    // ending. A Windows checkout holds the committed file under CRLF, and the generated
    // blocks emit LF — mixing the two would make this output disagree with
    // `fold_crlf(committed)` in the equality test. The committed file is LF, so this is a
    // no-op off Windows, on the same terms `crates/lanekeep-types-gen`'s renderer emits LF.
    let current = current.replace("\r\n", "\n");
    let mut out = current;
    out = replace_value(&out, "typesVersions", &types_versions_block());
    out = replace_value(&out, "exports", &exports_block());
    out
}

/// Render `packages/lanekeep/types.test-d.ts` — the `tsc` gate.
///
/// One `@ts-expect-error` import per component built-in (must fail to resolve) and one plain
/// import per module built-in (must compile as `Rule & ((options?) => Rule)`). Fully derived
/// from `COMPONENT_RULES`, so a migration adds or removes a line without a hand edit.
pub fn render_types_test_dts() -> String {
    let mut out = String::new();
    out.push_str("/**\n");
    out.push_str(" * Generated from `COMPONENT_RULES` — do not edit by hand. ");
    out.push_str("Run `just generate-builtin-subpaths`.\n");
    out.push_str(" *\n");
    out.push_str(" * A component built-in has no module to import, so each `@ts-expect-error` ");
    out.push_str("below must fire.\n");
    out.push_str(" * A module built-in imports as `Rule & ((options?) => Rule)`.\n");
    out.push_str(" */\n\n");

    for name in lanekeep_rules::names() {
        if lanekeep_rules::is_declared_component(name) {
            // `writeln!` into a `String` is infallible; the result is discarded on the same
            // terms as `crates/lanekeep-types-gen/src/lib.rs`'s renderers.
            let _ = writeln!(
                out,
                "// @ts-expect-error {name} is a component, not importable\nimport {} from 'lanekeep/{name}'\n",
                camel(name)
            );
        }
    }

    let modules: Vec<&str> = lanekeep_rules::names()
        .filter(|n| !lanekeep_rules::is_declared_component(n))
        .collect();
    for name in modules {
        let _ = writeln!(
            out,
            "import {ident} from 'lanekeep/{name}'",
            ident = camel(name)
        );
    }
    out.push('\n');
    for name in lanekeep_rules::names().filter(|n| !lanekeep_rules::is_declared_component(n)) {
        let _ = writeln!(out, "void {}", camel(name));
    }
    out
}

/// The `exports` value text — the object literal that follows `"exports": `, indented for a
/// top-level key (entries at four spaces, the closing brace at two).
///
/// The static subpaths (the package root, the shared `paths` module, the runtime subpaths and
/// the `./package.json` self-reference) are owned here alongside the built-in rule subpaths:
/// they are the importable surface `package.json` declares, and generating the whole value
/// keeps it a fixed point of the renderer. A new static entry is a change here, which
/// `tests/generated.rs` makes visible rather than silent.
fn exports_block() -> String {
    let mut entries: Vec<String> = vec![
        "    \".\": {\n      \"types\": \"./index.d.ts\",\n      \"default\": \"./index.js\"\n    }"
            .to_owned(),
        "    \"./paths\": \"./modules/paths.ts\"".to_owned(),
        "    \"./runtime/resolve\": \"./runtime/resolve.js\"".to_owned(),
        "    \"./runtime/entry\": \"./runtime/entry.js\"".to_owned(),
        "    \"./runtime/host\": \"./runtime/host.js\"".to_owned(),
        "    \"./package.json\": \"./package.json\"".to_owned(),
    ];
    for name in lanekeep_rules::names().filter(|n| !lanekeep_rules::is_declared_component(n)) {
        entries.push(format!(
            "    \"./{name}\": {{\n      \"types\": \"./{BUILTIN_DTS}\"\n    }}"
        ));
    }
    format!("{{\n{}\n  }}", entries.join(",\n"))
}

/// The `typesVersions` value text — the legacy fallback that mirrors `exports` for older
/// TypeScript. Component built-ins are omitted for the same reason.
fn types_versions_block() -> String {
    let inner: Vec<String> = lanekeep_rules::names()
        .filter(|n| !lanekeep_rules::is_declared_component(n))
        .map(|n| format!("      \"{n}\": [\"{BUILTIN_DTS}\"]"))
        .collect();
    format!("{{\n    \"*\": {{\n{}\n    }}\n  }}", inner.join(",\n"))
}

/// Replace the JSON value assigned to `key` in `text` with `new_value`, leaving everything
/// else byte-for-byte. Panics if `key` is not present — `package.json` must declare both
/// managed fields.
fn replace_value(text: &str, key: &str, new_value: &str) -> String {
    let (start, end) = find_value_span(text, key)
        .unwrap_or_else(|| panic!("`packages/lanekeep/package.json` has no `{key}` field"));
    let mut out = String::with_capacity(text.len() + new_value.len());
    out.push_str(&text[..start]);
    out.push_str(new_value);
    out.push_str(&text[end..]);
    out
}

/// The byte span `[start, end)` of the value assigned to `key` in `text`, where `start` is the
/// value's first character and `end` is one past its last.
fn find_value_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let needle = format!("\"{key}\"");
    let bytes = text.as_bytes();
    let mut from = 0;
    while from <= bytes.len() {
        let idx = text[from..].find(&needle)? + from;
        let mut j = idx + needle.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b':' {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let value_start = j;
            let value_end = value_end(text, value_start)?;
            return Some((value_start, value_end));
        }
        from = idx + needle.len();
    }
    None
}

/// The index one past the end of the JSON value starting at `start`.
fn value_end(text: &str, start: usize) -> Option<usize> {
    match text.as_bytes()[start] {
        b'{' | b'[' => brace_match(text, start),
        b'"' => string_end(text, start),
        _ => {
            // A scalar (bool, number, null): runs until a delimiter or whitespace.
            let bytes = text.as_bytes();
            let mut k = start;
            while k < bytes.len()
                && !matches!(bytes[k], b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
            {
                k += 1;
            }
            Some(k)
        }
    }
}

/// The index one past the `}` or `]` that closes the object/array opening at `open`.
fn brace_match(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut in_str = false;
    let mut esc = false;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' || c == b'[' {
            depth += 1;
        } else if c == b'}' || c == b']' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

/// The index one past the closing `"` of the string opening at `start`.
fn string_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut esc = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'"' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Camel-case a kebab-case rule name for a TypeScript identifier: `no-unwrap` -> `noUnwrap`.
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut capitalize = false;
    for c in name.chars() {
        if c == '-' {
            capitalize = true;
        } else if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_cases_kebab_names() {
        assert_eq!(camel("no-unwrap"), "noUnwrap");
        assert_eq!(camel("no-broad-except"), "noBroadExcept");
        assert_eq!(
            camel("no-mutable-default-argument"),
            "noMutableDefaultArgument"
        );
        assert_eq!(camel("paths"), "paths");
    }

    #[test]
    fn the_renderer_is_a_fixed_point_on_its_own_output() {
        let once = render_package_json(EMPTY);
        let twice = render_package_json(&once);
        assert_eq!(once, twice, "running the renderer twice must not drift");
    }

    #[test]
    fn the_renderer_normalizes_crlf_so_a_windows_checkout_stays_a_fixed_point() {
        // A Windows checkout holds the committed file under CRLF. The renderer must fold that
        // to LF — the generated blocks emit LF, and mixing the two would disagree with
        // `fold_crlf(committed)` in the equality test (this is exactly what reddened the
        // Windows CI job before the normalization).
        let crlf = EMPTY.replace('\n', "\r\n");
        let rendered = render_package_json(&crlf);
        assert!(
            !rendered.contains("\r\n"),
            "the rendered file must be LF, not a mix of CRLF and LF"
        );
        assert_eq!(rendered, render_package_json(EMPTY));
    }

    #[test]
    fn component_builtins_are_absent_and_module_builtins_present() {
        let rendered = render_package_json(EMPTY);
        for name in lanekeep_rules::names() {
            let key = format!("\"./{name}\"");
            if lanekeep_rules::is_declared_component(name) {
                assert!(
                    !rendered.contains(&key),
                    "component built-in `{name}` must not be in exports"
                );
            } else {
                assert!(
                    rendered.contains(&key),
                    "module built-in `{name}` must be in exports"
                );
            }
        }
        assert!(
            !rendered.contains("\"./*\""),
            "the `\"./*\"` catch-all must be gone"
        );
        // The non-managed fields are preserved byte-for-byte.
        assert!(rendered.contains("\"name\": \"lanekeep\""));
        assert!(rendered.contains("\"typescript\": \"5.9.3\""));
    }

    #[test]
    fn types_test_dts_marks_every_component_and_imports_every_module() {
        let rendered = render_types_test_dts();
        for name in lanekeep_rules::names() {
            let import = format!("import {} from 'lanekeep/{name}'", camel(name));
            assert!(rendered.contains(&import), "missing import for `{name}`");
            if lanekeep_rules::is_declared_component(name) {
                let directive = format!("@ts-expect-error {name} is a component");
                assert!(
                    rendered.contains(&directive),
                    "component `{name}` must be under @ts-expect-error"
                );
            }
        }
    }

    /// A minimal `package.json` with the catch-all and a stray devDependency, to prove the
    /// renderer removes the catch-all and preserves the rest.
    const EMPTY: &str = "{\n  \"name\": \"lanekeep\",\n  \"private\": true,\n  \"types\": \"index.d.ts\",\n  \"typesVersions\": {\n    \"*\": {\n      \"*\": [\"builtin.d.ts\"]\n    }\n  },\n  \"exports\": {\n    \".\": {\n      \"types\": \"./index.d.ts\",\n      \"default\": \"./index.js\"\n    },\n    \"./*\": {\n      \"types\": \"./builtin.d.ts\"\n    }\n  },\n  \"devDependencies\": {\n    \"typescript\": \"5.9.3\"\n  }\n}\n";
}
