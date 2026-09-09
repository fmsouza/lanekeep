//! Turning a module specifier into a file this provider may read.
//!
//! Node-shaped and no wider than a declaration lookup needs. Nothing existing is reusable:
//! `RuleRoot::resolve` is anchored at the rules root and refuses bare specifiers, and the
//! cross-file rules' `resolveImport` resolves only into the discovered corpus, which never
//! contains `node_modules` because discovery honors gitignore.
//!
//! # Every probe is a tracked read
//!
//! Hit or miss, through the caller's [`FileAccess`], so an absent `dist/index.d.ts` is
//! recorded with a null hash and its later appearance invalidates the importing file's cache
//! entry. Probe order is fixed, so the recorded dependency list is a function of the input
//! rather than of which candidate happened to exist.
//!
//! # Nothing above the project root, ever
//!
//! The walk stops at the root, and the two ways past it fail differently.
//!
//! A `node_modules` **hoisted above the root** — a monorepo checked from a package directory —
//! is never probed at all: the walk simply stops, so no path above the root is read and none
//! appears in the dependency list. In-root candidates are probed and recorded as absent, the
//! answer is `None`, and the importing file is reported incomplete.
//!
//! A package **symlinked out of the root** — the pnpm store, reached through an in-root
//! `node_modules/pkg` — *is* probed, because the path names something inside the root. The
//! read is refused after canonicalizing, and `FileAccess` records the refusal as an absent
//! dependency, so the day that path becomes a real in-root file the answer that rested on the
//! refusal is invalidated rather than served forever.
//!
//! The remedy for both is the same: point lanekeep at the workspace root — the directory
//! `node_modules` lives in — rather than at a package inside it. `--config` does not move the
//! root. It is in `docs/type-aware-rules.md`.

use std::path::Path;

use lanekeep_core::FilePath;
use lanekeep_core::files::{FileAccess, normalize};

/// Extensions tried for a relative specifier, in order.
///
/// The source file before the declaration file: a `.d.ts` beside a `.ts` in one tree is a
/// build artifact that can be stale, and the source is what the program means.
///
/// **`.tsx` and `.jsx` are deliberately absent, and that costs more than a declaration file.**
/// This provider parses everything it opens with the TypeScript grammar — chosen by name, see
/// `provider_language` in `lanekeep-engine` — under which every JSX element is an `ERROR` node
/// with no error reported anywhere, the trap that produced 2218 false positives in one rule. A
/// file this cannot read honestly is one it does not read.
///
/// What that gives up is **project sources**, not declaration files. A declaration file is
/// never TSX, so nothing is lost in `node_modules`; but `import { Button } from './Button'`
/// with `Button.tsx` beside it matches none of the suffixes above, records six absent reads,
/// answers `undefined` for every name it brought in, and makes the importing file
/// `complete() == false`. On a React codebase that is most sibling imports. A stated
/// limitation rather than a bug to be surprised by — the refinement is filed with the
/// resolver's own issue.
const RELATIVE_SUFFIXES: &[&str] = &[".ts", ".mts", ".cts", ".d.ts", "/index.ts", "/index.d.ts"];

/// Resolve `specifier`, written in `from`, to a file inside the project root.
///
/// `None` when nothing readable answers it, which is an ordinary result rather than a
/// failure: the type answer that needed it is then `undefined` and the importing file is
/// incomplete.
#[must_use]
pub fn resolve_specifier(files: &FileAccess, from: &FilePath, specifier: &str) -> Option<FilePath> {
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return relative(files, from, specifier);
    }
    // A rooted specifier leaves the project by construction and a bare `.` or `..` is not a
    // module. Neither is probed, so neither is recorded — the same reason `FileAccess` records
    // only the *symlink* refusal and not a lexical one: a path that can never name something
    // inside the root is not a dependency any future filesystem state can make relevant.
    if specifier.starts_with('/') || specifier.starts_with('.') || specifier.is_empty() {
        return None;
    }
    bare(files, from, specifier)
}

/// A specifier resolved against the importing file's own directory.
fn relative(files: &FileAccess, from: &FilePath, specifier: &str) -> Option<FilePath> {
    // TypeScript's ESM spelling names the *emitted* file; the declaration sits at the same
    // stem. Stripping the suffix here rather than adding four more probe entries keeps the
    // recorded dependency list short, which is a cache-entry-size decision as much as a
    // correctness one.
    let stem = [".js", ".mjs", ".cjs"]
        .iter()
        .find_map(|suffix| specifier.strip_suffix(suffix))
        .unwrap_or(specifier);

    let base = within_root(&join(parent_of(from.as_str()), stem))?;
    for suffix in RELATIVE_SUFFIXES {
        let candidate = format!("{base}{suffix}");
        if files.exists(&candidate).unwrap_or(false) {
            return Some(FilePath::new(&candidate));
        }
    }
    None
}

/// Everything before the last `/`, or the empty string for a file at the root.
fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(at) => &path[..at],
        None => "",
    }
}

/// Join two project-relative fragments with `/`, tolerating an empty left side.
fn join(left: &str, right: &str) -> String {
    if left.is_empty() {
        right.to_owned()
    } else {
        format!("{left}/{right}")
    }
}

/// Collapse `.` and `..` lexically, refusing anything that ends up above the root.
///
/// [`normalize`] keeps a leading `..` as a marker precisely so a caller can see it — see its
/// own documentation for why a later `..` must not pop that marker. `FileAccess` would refuse
/// such a path anyway; refusing it here is what keeps it from being *probed*, so an escape
/// attempt records nothing at all.
fn within_root(path: &str) -> Option<String> {
    let normalized = normalize(Path::new(path))
        .to_string_lossy()
        .replace('\\', "/");
    if normalized.is_empty() || normalized == ".." || normalized.starts_with("../") {
        return None;
    }
    Some(normalized)
}

/// A bare specifier, resolved by walking `node_modules` upward and stopping at the root.
fn bare(files: &FileAccess, from: &FilePath, specifier: &str) -> Option<FilePath> {
    let (package, subpath) = split_specifier(specifier)?;
    let types_package = at_types_name(&package);

    let mut directory = parent_of(from.as_str()).to_owned();
    loop {
        for name in [package.as_str(), types_package.as_str()] {
            let root = join(&directory, &format!("node_modules/{name}"));
            if let Some(found) = in_package(files, &root, &subpath) {
                return Some(found);
            }
        }
        if directory.is_empty() {
            // The project root. Nothing above it is readable, ever.
            return None;
        }
        directory = parent_of(&directory).to_owned();
    }
}

/// Split `@scope/name/deep/path` into the package and the subpath after it.
///
/// `None` for an empty package name, which is not a specifier any resolver should probe for.
fn split_specifier(specifier: &str) -> Option<(String, String)> {
    let scoped = specifier.starts_with('@');
    let mut parts = specifier.splitn(if scoped { 3 } else { 2 }, '/');
    let first = parts.next()?;
    if first.is_empty() {
        return None;
    }
    if scoped {
        let name = parts.next()?;
        if name.is_empty() {
            return None;
        }
        Some((
            format!("{first}/{name}"),
            parts.next().unwrap_or_default().to_owned(),
        ))
    } else {
        Some((
            first.to_owned(),
            parts.next().unwrap_or_default().to_owned(),
        ))
    }
}

/// The DefinitelyTyped package for a name: `@scope/x` flattens to `@types/scope__x`.
fn at_types_name(package: &str) -> String {
    match package.strip_prefix('@') {
        Some(rest) => format!("@types/{}", rest.replacen('/', "__", 1)),
        None => format!("@types/{package}"),
    }
}

/// Find a declaration file inside one package directory.
///
/// `exports` first, then `types`, then `typings`, then the conventional index. That is the
/// order TypeScript itself resolves in, and a fixed order is what makes the recorded
/// dependency list a function of the input rather than of which file happened to exist.
fn in_package(files: &FileAccess, root: &str, subpath: &str) -> Option<FilePath> {
    if let Ok(Some(text)) = files.read(&join(root, "package.json"))
        && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text)
    {
        if let Some(target) = exports_target(&manifest, subpath)
            && let Some(found) = candidate(files, root, &target)
        {
            return Some(found);
        }
        if subpath.is_empty() {
            for field in ["types", "typings"] {
                if let Some(target) = manifest.get(field).and_then(serde_json::Value::as_str)
                    && let Some(found) = candidate(files, root, target)
                {
                    return Some(found);
                }
            }
        }
    }

    // No manifest, or a manifest that says nothing about types. A package without one still
    // ships `index.d.ts` far more often than not, and `@types/*` packages ship nothing else.
    let fallback = if subpath.is_empty() {
        "index.d.ts".to_owned()
    } else {
        format!("{subpath}/index.d.ts")
    };
    candidate(files, root, &fallback)
}

/// Probe one target inside a package directory.
///
/// The extra spellings are tried only for a target with no `.ts` extension, which keeps a
/// `"types": "./index.d.ts"` to a single recorded read rather than three. Entry size is the
/// reason: every probe is a dependency, and a package resolved on every file that imports it
/// multiplies whatever this costs.
fn candidate(files: &FileAccess, root: &str, target: &str) -> Option<FilePath> {
    let target = target.strip_prefix("./").unwrap_or(target);
    let mut spellings = vec![target.to_owned()];
    if !Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
    {
        spellings.push(format!("{target}.d.ts"));
        spellings.push(format!("{target}/index.d.ts"));
    }
    for spelling in spellings {
        let Some(path) = within_root(&join(root, &spelling)) else {
            continue;
        };
        if files.exists(&path).unwrap_or(false) {
            return Some(FilePath::new(&path));
        }
    }
    None
}

/// The `exports` entry for a subpath, read through the `types` condition.
///
/// Exact keys before `*` patterns, and the longest literal prefix before a shorter one, which
/// is what Node itself specifies — `./deep/*` has to beat `./*` or a package that publishes
/// both resolves to the wrong file.
fn exports_target(manifest: &serde_json::Value, subpath: &str) -> Option<String> {
    let exports = manifest.get("exports")?;
    let key = if subpath.is_empty() {
        ".".to_owned()
    } else {
        format!("./{subpath}")
    };

    // A string, or an object with no subpath keys at all, is the `.` entry written short.
    let subpaths = exports
        .as_object()
        .is_some_and(|map| map.keys().any(|k| k.starts_with('.')));
    if !subpaths {
        return if key == "." {
            types_condition(exports)
        } else {
            None
        };
    }

    let map = exports.as_object()?;
    if let Some(target) = map.get(&key).and_then(types_condition) {
        return Some(target);
    }

    let mut patterns: Vec<(&String, &serde_json::Value)> =
        map.iter().filter(|(k, _)| k.contains('*')).collect();
    // Longest key first, and the key itself as the tiebreak so two keys of one length cannot
    // depend on iteration order. `serde_json`'s `Map` is a `BTreeMap` here — the workspace
    // does not enable `preserve_order` — so the input order is already sorted, and this makes
    // the dependence on that explicit rather than assumed.
    patterns.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
    for (pattern, value) in patterns {
        if let Some(matched) = star_match(pattern, &key)
            && let Some(target) = types_condition(value)
        {
            return Some(target.replace('*', &matched));
        }
    }
    None
}

/// The `types` condition of an export entry, however deeply it is nested.
///
/// Only `types`. A package that publishes its declarations solely under `default` or `import`
/// resolves to nothing here, which is the honest answer for a resolver that cannot read
/// JavaScript: following `default` would hand this provider a `.js` file to parse as
/// TypeScript, and the cost of that mistake is a confidently wrong type rather than none.
///
/// The nested search iterates a `BTreeMap` — see [`exports_target`] — so a manifest with two
/// nested condition objects resolves the same way on every run.
fn types_condition(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(target) => Some(target.clone()),
        serde_json::Value::Object(map) => map.get("types").and_then(under_types).or_else(|| {
            map.values().find_map(|nested| match nested {
                serde_json::Value::Object(_) => types_condition(nested),
                _ => None,
            })
        }),
        _ => None,
    }
}

/// The file named *under* a `types` condition, which may itself be a conditions object.
///
/// `{"types": {"import": "./index.d.mts", "require": "./index.d.ts"}}` is the shape a package
/// shipping both module systems publishes. Once inside `types`, every leaf is a declaration
/// file whatever condition names it, so a string leaf is accepted here where
/// [`types_condition`] refuses one — outside `types`, `default` and `import` name the emitted
/// JavaScript, and following one would hand this provider a `.js` file to parse as TypeScript.
///
/// The order is the map's own: `serde_json`'s `Map` is a `BTreeMap` here, so the conditions are
/// visited in sorted key order and a package publishing several resolves the same way on every
/// run. Which one is chosen is arbitrary in the sense that Node would consult the *importer*'s
/// module system to decide; both name declarations for the same package, and a fixed choice is
/// what keeps the recorded dependency list a function of the input.
fn under_types(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(target) => Some(target.clone()),
        serde_json::Value::Object(map) => map
            .get("types")
            .and_then(under_types)
            .or_else(|| map.values().find_map(under_types)),
        _ => None,
    }
}

/// What `*` stood for, when `pattern` matches `key`.
fn star_match(pattern: &str, key: &str) -> Option<String> {
    let (head, tail) = pattern.split_once('*')?;
    let rest = key.strip_prefix(head)?;
    Some(rest.strip_suffix(tail)?.to_owned())
}
