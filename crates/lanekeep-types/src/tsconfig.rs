//! A `tsconfig.json`'s `baseUrl` and `paths`, read the way TypeScript reads them.
//!
//! The resolver's half of #281. `lanekeep/paths` (`crates/lanekeep-rules/modules/paths.ts`)
//! does the same job for the cross-file rules, in JavaScript and inside the sandbox; this is
//! the builtin type provider's copy, and the two are meant to agree on everything below.
//!
//! # Every read is a tracked read
//!
//! The nearest config, every `tsconfig.json` looked for and not found on the way up to it, and
//! every link of its `extends` chain are read through the caller's [`FileAccess`]. So each is a
//! dependency of the file whose answer used it, and editing `paths`, adding a nearer config, or
//! repairing a broken one invalidates exactly the files that resolved a bare specifier under
//! it. The walk order is fixed, so the recorded list is a function of the input.
//!
//! # What is followed, and what is not
//!
//! Followed: a relative `extends`, a string or an array, as written and then with `.json`
//! appended; `compilerOptions` merged key by key, later wins, so an extending config's `paths`
//! replaces its base's wholesale. Comments, trailing commas and a byte-order mark, which `tsc`
//! accepts. `baseUrl` relative to the config declaring it; `paths` substitutions relative to the
//! effective `baseUrl`, or to the directory of the config declaring `paths` when there is none.
//!
//! Not followed: a package-name `extends` (`@tsconfig/node20`), an `extends` that leaves the
//! project root, `jsconfig.json`, `tsconfig.*.json`, `rootDirs`, and TypeScript 5.5's
//! `${configDir}` template, which is read as a literal directory name. Each is skipped as
//! though absent rather than refused, so a project whose base config lives above the root still
//! resolves its packages through `node_modules` the way it did before any of this existed.
//!
//! A `baseUrl` or `paths` substitution that points above the root is a different matter: `tsc`
//! would look there, so a matched substitution that does ends the resolution with no answer —
//! see [`Candidate::Unreachable`].
//!
//! # An unreadable config answers nothing
//!
//! A config, or a link of its chain, that is not valid JSON — or not an object, or not text, or
//! a symlink out of the root — makes the whole lookup [`Lookup::Unreadable`], and the resolver
//! then answers `None` for every bare specifier beneath it. Ignoring it instead would fall back
//! to `node_modules` alone, and an alias the config would have mapped elsewhere — `"lodash":
//! ["./src/shims/lodash"]` — would resolve to a *different* file: a confidently wrong type, where
//! the provider's contract is to answer nothing when it cannot be sure.

use std::ffi::OsStr;
use std::path::Path;

use lanekeep_core::files::FileAccess;
use serde_json::{Map, Value};

use crate::resolve::{join, parent_of};

/// What the nearest `tsconfig.json` says, if anything.
#[derive(Debug)]
pub(crate) enum Lookup {
    /// No `tsconfig.json` between the importing file and the project root.
    Absent,
    /// One was found, and it or a link of its `extends` chain could not be read.
    Unreadable,
    /// The `baseUrl` and `paths` in force.
    Found(Options),
}

/// The two options module resolution reads, made project-relative.
#[derive(Debug, Default)]
pub(crate) struct Options {
    /// `compilerOptions.baseUrl`; `None` when no config in the chain sets one.
    base_url: Option<BaseUrl>,
    /// `compilerOptions.paths`, and the directory of the config that declared it.
    paths: Option<(Map<String, Value>, String)>,
}

/// Where a `baseUrl` points.
///
/// Three states rather than an `Option<String>`, because "above the root" and "unset" mean
/// different things and folding one into the other was a bug: `paths` resolve against the
/// config's own directory only when there is *no* `baseUrl`, so an out-of-root one read as
/// unset rebased every substitution onto a directory `tsc` never looks in.
#[derive(Debug)]
enum BaseUrl {
    /// Relative to the project root; `""` is the root itself.
    At(String),
    /// Above the project root, where nothing may be read.
    Outside,
}

/// One place TypeScript would look for a bare specifier.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Candidate {
    /// A project-relative base path, probed like a relative specifier.
    At(String),
    /// A `paths` substitution that resolves above the project root. `tsc` would look there
    /// before any later candidate, and this provider cannot, so the resolution stops with no
    /// answer rather than going on to one `tsc` might never have reached.
    Unreachable,
}

/// The options in force for a file in `directory`, from the nearest `tsconfig.json`.
///
/// Walks up from `directory` to the project root, never past it.
pub(crate) fn nearest(files: &FileAccess, directory: &str) -> Lookup {
    let mut at = directory.to_owned();
    loop {
        let path = join(&at, "tsconfig.json");
        match files.read(&path) {
            Ok(Some(text)) => {
                return match load(files, &path, &text, std::slice::from_ref(&path)) {
                    Some(options) => Lookup::Found(options),
                    None => Lookup::Unreadable,
                };
            }
            Ok(None) => {}
            // Not text, or a symlink out of the root: there is a config here and it cannot be
            // read, which is exactly the case an unparsable one is.
            Err(_) => return Lookup::Unreadable,
        }
        if at.is_empty() {
            return Lookup::Absent;
        }
        at = parent_of(&at).to_owned();
    }
}

impl Options {
    /// Where TypeScript would look for a bare `specifier`, in order.
    ///
    /// An exact `paths` key wins; otherwise the wildcard key with the longest prefix that
    /// matches. Its substitutions come in the order written, `*` replaced by what it matched,
    /// and then `<baseUrl>/<specifier>` when a `baseUrl` is set. Nothing above the root is ever
    /// a path here, so nothing above it is probed or recorded; but a matched substitution that
    /// resolves there is [`Candidate::Unreachable`] rather than dropped, because the answer
    /// `tsc` gives may be the file it names. The `<baseUrl>/<specifier>` candidate of an
    /// out-of-root `baseUrl` is dropped instead: it is tried for every package import, and
    /// stopping there would leave a project rooted below its `baseUrl` with no package
    /// resolving at all.
    pub(crate) fn candidates(&self, specifier: &str) -> Vec<Candidate> {
        let mut out = Vec::new();

        if let Some((paths, declared_in)) = &self.paths
            && let Some((key, star)) = match_pattern(paths, specifier)
            && let Some(Value::Array(substitutions)) = paths.get(key)
        {
            let base = match &self.base_url {
                Some(BaseUrl::At(base)) => Some(base.as_str()),
                Some(BaseUrl::Outside) => None,
                None => Some(declared_in.as_str()),
            };
            for substitution in substitutions {
                let Value::String(substitution) = substitution else {
                    continue;
                };
                // An empty match is TypeScript's own behavior too: it tests the match for
                // truthiness, so the substitution is used as written, `*` and all.
                let path = match star {
                    Some(star) if !star.is_empty() => replace_star(substitution, star),
                    _ => substitution.clone(),
                };
                match base.and_then(|base| within(base, &path)) {
                    // The root itself names no module.
                    Some(candidate) if candidate.is_empty() => {}
                    Some(candidate) => out.push(Candidate::At(candidate)),
                    None => {
                        out.push(Candidate::Unreachable);
                        return out;
                    }
                }
            }
        }

        if let Some(BaseUrl::At(base)) = &self.base_url {
            out.extend(
                within(base, specifier)
                    .filter(|c| !c.is_empty())
                    .map(Candidate::At),
            );
        }

        out
    }
}

/// One config's options after its `extends` chain, or `None` when any link is unreadable.
///
/// `chain` holds the configs already being read, so a cycle in `extends` stops at the repeat.
fn load(files: &FileAccess, path: &str, text: &str, chain: &[String]) -> Option<Options> {
    let config = parse(text)?;
    let directory = parent_of(path);
    let mut options = Options::default();

    let parents: Vec<&Value> = match config.get("extends") {
        Some(Value::Array(parents)) => parents.iter().collect(),
        Some(parent) => vec![parent],
        None => Vec::new(),
    };
    for parent in parents {
        match extended(files, directory, parent, chain) {
            Parent::Skipped => {}
            Parent::Unreadable => return None,
            Parent::Loaded(inherited) => {
                if inherited.base_url.is_some() {
                    options.base_url = inherited.base_url;
                }
                if inherited.paths.is_some() {
                    options.paths = inherited.paths;
                }
            }
        }
    }

    if let Some(Value::Object(own)) = config.get("compilerOptions") {
        if let Some(Value::String(base_url)) = own.get("baseUrl") {
            options.base_url = Some(match within(directory, base_url) {
                Some(base_url) => BaseUrl::At(base_url),
                None => BaseUrl::Outside,
            });
        }
        if let Some(Value::Object(paths)) = own.get("paths") {
            options.paths = Some((paths.clone(), directory.to_owned()));
        }
    }

    Some(options)
}

/// What one `extends` entry contributed.
enum Parent {
    /// Not followed: not a relative path, leaving the root, part of a cycle, or naming nothing.
    Skipped,
    /// It names a config that cannot be read, which makes the whole lookup unreadable.
    Unreadable,
    /// The options it carries, after its own chain.
    Loaded(Options),
}

/// The options of the config an `extends` entry names.
fn extended(files: &FileAccess, directory: &str, parent: &Value, chain: &[String]) -> Parent {
    let Value::String(specifier) = parent else {
        return Parent::Skipped;
    };
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        return Parent::Skipped;
    }
    let Some(written) = within(directory, specifier).filter(|w| !w.is_empty()) else {
        return Parent::Skipped;
    };

    // As written first, then with `.json` appended: the order TypeScript tries, and as
    // case-sensitive as its own check.
    let mut candidates = vec![written.clone()];
    if Path::new(&written).extension() != Some(OsStr::new("json")) {
        candidates.push(format!("{written}.json"));
    }
    for path in candidates {
        if chain.contains(&path) {
            return Parent::Skipped;
        }
        match files.read(&path) {
            Ok(Some(text)) => {
                let mut longer = chain.to_vec();
                longer.push(path.clone());
                return match load(files, &path, &text, &longer) {
                    Some(options) => Parent::Loaded(options),
                    None => Parent::Unreadable,
                };
            }
            Ok(None) => {}
            Err(_) => return Parent::Unreadable,
        }
    }
    Parent::Skipped
}

/// A config's text as a JSON object, or `None` when it is not one.
fn parse(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(&strip_jsonc(text)?) {
        Ok(Value::Object(config)) => Some(config),
        _ => None,
    }
}

/// The `paths` key `specifier` matches, and what its `*` stood for.
///
/// An exact key wins outright. Among wildcard keys — exactly one `*`; TypeScript rejects more —
/// the longest prefix wins. TypeScript breaks a tie by declaration order; `serde_json`'s map is
/// a `BTreeMap` in this workspace and does not keep it, so a tie goes to the key that sorts
/// first. Only reachable when two keys share a prefix length and both match one specifier.
fn match_pattern<'m, 's>(
    paths: &'m Map<String, Value>,
    specifier: &'s str,
) -> Option<(&'m str, Option<&'s str>)> {
    if let Some((key, _)) = paths.get_key_value(specifier)
        && !key.contains('*')
    {
        return Some((key.as_str(), None));
    }

    let mut best: Option<(&'m str, usize)> = None;
    for key in paths.keys() {
        let Some((prefix, suffix)) = key.split_once('*') else {
            continue;
        };
        if suffix.contains('*') {
            continue;
        }
        if specifier.len() < prefix.len() + suffix.len()
            || !specifier.starts_with(prefix)
            || !specifier.ends_with(suffix)
        {
            continue;
        }
        if best.is_none_or(|(_, longest)| prefix.len() > longest) {
            best = Some((key.as_str(), prefix.len()));
        }
    }

    let (key, _) = best?;
    let (prefix, suffix) = key.split_once('*')?;
    // The bounds were checked above, against this same key.
    let star = specifier.get(prefix.len()..specifier.len() - suffix.len())?;
    Some((key, Some(star)))
}

/// `substitution` with its first `*` replaced by `star`.
fn replace_star(substitution: &str, star: &str) -> String {
    match substitution.split_once('*') {
        Some((head, tail)) => format!("{head}{star}{tail}"),
        None => substitution.to_owned(),
    }
}

/// `relative` joined onto `directory`, lexically, or `None` when it leaves the project root.
///
/// Unlike [`crate::resolve`]'s `within_root`, the root itself is an answer here: `baseUrl: "."`
/// in the root config is the root, and is `""`.
fn within(directory: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') {
        return None;
    }
    let mut segments: Vec<&str> = if directory.is_empty() {
        Vec::new()
    } else {
        directory.split('/').collect()
    };
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    Some(segments.join("/"))
}

/// JSON with comments and trailing commas, which is what a tsconfig is, as plain JSON.
///
/// Two passes, both aware of string literals: a `//` inside `"https://..."` is text, not a
/// comment. The first drops comments; the second drops a comma whose next non-blank byte closes
/// an object or an array, which it can only see once the comments between them are gone. A
/// leading byte-order mark is dropped too. Byte-wise, which is safe because every byte it
/// inspects is ASCII and no byte of a multi-byte UTF-8 sequence is.
fn strip_jsonc(text: &str) -> Option<String> {
    let bytes = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();

    let mut uncommented = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        match (byte, bytes.get(i + 1)) {
            (b'"', _) => {
                let end = end_of_string(bytes, i);
                uncommented.extend_from_slice(bytes.get(i..end)?);
                i = end;
            }
            (b'/', Some(b'/')) => {
                while bytes.get(i).is_some_and(|&b| b != b'\n') {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i = bytes
                    .get(i + 2..)?
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(bytes.len(), |at| i + 2 + at + 2);
                uncommented.push(b' ');
            }
            _ => {
                uncommented.push(byte);
                i += 1;
            }
        }
    }

    let mut out = Vec::with_capacity(uncommented.len());
    let mut i = 0;
    while let Some(&byte) = uncommented.get(i) {
        if byte == b'"' {
            let end = end_of_string(&uncommented, i);
            out.extend_from_slice(uncommented.get(i..end)?);
            i = end;
            continue;
        }
        if byte == b',' {
            let mut next = i + 1;
            while uncommented.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            if matches!(uncommented.get(next), Some(b'}' | b']')) {
                i += 1;
                continue;
            }
        }
        out.push(byte);
        i += 1;
    }

    String::from_utf8(out).ok()
}

/// The index just past the string literal opening at `start`, clamped to the input.
fn end_of_string(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'"' {
            return i + 1;
        }
        i += if byte == b'\\' { 2 } else { 1 };
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::{replace_star, strip_jsonc, within};

    #[test]
    fn jsonc_keeps_slashes_inside_strings() {
        assert_eq!(
            strip_jsonc("{\"a\": \"https://x\", // c\n \"b\": [1,],}").as_deref(),
            Some("{\"a\": \"https://x\", \n \"b\": [1]}")
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        assert_eq!(
            strip_jsonc(r#"{"a": "x\"//y"}"#).as_deref(),
            Some(r#"{"a": "x\"//y"}"#)
        );
    }

    #[test]
    fn within_refuses_to_leave_the_root_and_keeps_the_root_itself() {
        assert_eq!(within("", ".").as_deref(), Some(""));
        assert_eq!(within("a", "../b").as_deref(), Some("b"));
        assert_eq!(within("a", "../../b"), None);
        assert_eq!(within("a", "/etc"), None);
    }

    #[test]
    fn only_the_first_star_is_replaced() {
        assert_eq!(replace_star("src/*/*", "x"), "src/x/*");
        assert_eq!(replace_star("src/$&*", "x"), "src/$&x");
    }
}
