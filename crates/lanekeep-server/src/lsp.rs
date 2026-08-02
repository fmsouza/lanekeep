//! The Language Server Protocol surface.
//!
//! Diagnostics, published on open and on save. Not on every keystroke: a check reads files
//! from disk, and the buffer an editor holds mid-edit is not on disk yet. Publishing against
//! stale bytes would put squiggles under the wrong characters, which is worse than a short
//! delay — §12 already says the warm cache is what makes one-shot fast, and the same cache
//! makes a save-triggered re-check fast enough to feel immediate.
//!
//! # Positions
//!
//! **LSP counts lines and characters from zero. lanekeep counts from one.** Every diagnostic
//! crosses that boundary, and getting it wrong shifts every squiggle up a line and left a
//! column — visible, but easy to mistake for a rule reporting the wrong node. [`to_range`]
//! is the one place the conversion happens, and it is tested at line and column 1 where the
//! subtraction would underflow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lanekeep_core::{Severity, Violation};
use serde_json::{Value, json};

/// LSP severity numbers, which are not lanekeep's.
mod severity {
    pub(super) const ERROR: u8 = 1;
    pub(super) const WARNING: u8 = 2;
}

/// What the server tells the client it can do.
///
/// `textDocumentSync: 1` is full-document sync. The server does not use the text the client
/// sends — it re-reads from disk — but declaring `none` would stop some clients sending the
/// open and save notifications that trigger a check at all.
#[must_use]
pub fn capabilities() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": {
                "openClose": true,
                "change": 1,
                "save": { "includeText": false },
            },
        },
        "serverInfo": {
            "name": "lanekeep",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Convert a violation's one-based position into LSP's zero-based range.
///
/// The range is the single character at the position. lanekeep reports a point, not a span:
/// a violation's location is where to look, and inventing an end column from a node's width
/// would be a different claim than the one the rule made.
#[must_use]
pub fn to_range(violation: &Violation) -> Value {
    // Saturating, because line or column 1 is the common case and `1 - 1` is the answer,
    // while a hypothetical 0 must not wrap to `u32::MAX` and put the squiggle off-screen.
    let line = violation.location.position.line.saturating_sub(1);
    let character = violation.location.position.column.saturating_sub(1);

    json!({
        "start": { "line": line, "character": character },
        "end": { "line": line, "character": character + 1 },
    })
}

/// Convert a violation into an LSP diagnostic.
#[must_use]
pub fn to_diagnostic(violation: &Violation) -> Value {
    json!({
        "range": to_range(violation),
        "severity": match violation.severity {
            Severity::Error => severity::ERROR,
            // Anything not an error is advice. `off` never reaches here — a disabled rule
            // does not run — and mapping it to a warning rather than dropping it would be a
            // diagnostic nobody asked for.
            _ => severity::WARNING,
        },
        "source": "lanekeep",
        "code": violation.rule_id.to_string(),
        // Both lines, because an editor shows one hover and the remediation is the half that
        // says what to do. Splitting them across `message` and a related-information entry
        // hides the actionable half behind a click.
        "message": format!("{}\n{}", violation.message, violation.remediation),
    })
}

/// Group violations by the file they belong to, as absolute paths.
///
/// Every file the client has open needs a `publishDiagnostics`, including the ones with
/// nothing wrong — an empty list is how a diagnostic gets cleared, and skipping it leaves
/// yesterday's squiggle on a line the author already fixed.
#[must_use]
pub fn by_file(root: &Path, violations: &[Violation]) -> BTreeMap<PathBuf, Vec<Value>> {
    let mut grouped: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
    for violation in violations {
        grouped
            .entry(root.join(violation.location.file.as_str()))
            .or_default()
            .push(to_diagnostic(violation));
    }
    grouped
}

/// The path a `file://` URI refers to.
///
/// Percent-decoded, because a path with a space arrives as `%20` and comparing the encoded
/// form against a path from disk would never match.
#[must_use]
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;

    // `file:///a/b` on Unix leaves `/a/b`; a Windows URI leaves `/C:/a/b`, where the leading
    // slash is part of the URI and not of the path.
    let rest = if rest.len() > 2
        && rest.starts_with('/')
        && rest.as_bytes()[2] == b':'
        && rest.as_bytes()[1].is_ascii_alphabetic()
    {
        &rest[1..]
    } else {
        rest
    };

    Some(PathBuf::from(percent_decode(rest)))
}

/// The `file://` URI for a path.
#[must_use]
pub fn uri_from_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
    format!("file://{}", percent_encode(&text))
}

/// Decode `%XX` escapes. Anything malformed is left as written rather than dropped.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Encode the characters a URI cannot carry literally. `/` stays, being the separator.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use lanekeep_core::{FilePath, Location, Position, RuleId};

    use super::*;

    fn violation(line: u32, column: u32, severity: Severity) -> Violation {
        Violation {
            rule_id: "local/example".parse::<RuleId>().expect("valid"),
            location: Location::new(FilePath::new("src/a.ts"), Position::new(line, column)),
            message: "something".to_owned(),
            remediation: "do this".to_owned(),
            severity,
            fix: None,
        }
    }

    #[test]
    fn positions_convert_from_one_based_to_zero_based() {
        let range = to_range(&violation(9, 5, Severity::Error));
        assert_eq!(range["start"]["line"], 8);
        assert_eq!(range["start"]["character"], 4);
    }

    #[test]
    fn the_first_line_and_column_do_not_underflow() {
        // `1 - 1` is 0, which is right. What must not happen is a wrap to u32::MAX, which
        // would put the squiggle somewhere no editor will ever show.
        let range = to_range(&violation(1, 1, Severity::Error));
        assert_eq!(range["start"]["line"], 0);
        assert_eq!(range["start"]["character"], 0);
        assert_eq!(range["end"]["character"], 1);
    }

    #[test]
    fn severity_maps_to_the_lsp_numbers() {
        assert_eq!(
            to_diagnostic(&violation(1, 1, Severity::Error))["severity"],
            severity::ERROR
        );
        assert_eq!(
            to_diagnostic(&violation(1, 1, Severity::Warn))["severity"],
            severity::WARNING
        );
    }

    #[test]
    fn a_diagnostic_carries_the_rule_id_and_both_lines() {
        let diagnostic = to_diagnostic(&violation(1, 1, Severity::Error));
        assert_eq!(diagnostic["code"], "local/example");
        assert_eq!(diagnostic["source"], "lanekeep");
        let message = diagnostic["message"].as_str().expect("a string");
        assert!(message.contains("something"), "{message}");
        assert!(message.contains("do this"), "{message}");
    }

    #[test]
    fn violations_group_by_file_as_absolute_paths() {
        let root = Path::new("/project");
        let grouped = by_file(
            root,
            &[
                violation(1, 1, Severity::Error),
                violation(2, 1, Severity::Warn),
            ],
        );
        assert_eq!(grouped.len(), 1);
        assert_eq!(
            grouped.keys().next().expect("one file"),
            Path::new("/project/src/a.ts")
        );
        assert_eq!(grouped.values().next().expect("one file").len(), 2);
    }

    #[test]
    fn a_uri_round_trips_through_a_path() {
        for path in ["/project/src/a.ts", "/project/with space/b.ts"] {
            let uri = uri_from_path(Path::new(path));
            assert_eq!(
                path_from_uri(&uri).as_deref(),
                Some(Path::new(path)),
                "{uri}"
            );
        }
    }

    #[test]
    fn a_space_is_percent_encoded_and_decoded() {
        // An editor sends `%20`; comparing that against a path read from disk never matches.
        assert_eq!(uri_from_path(Path::new("/a b/c.ts")), "file:///a%20b/c.ts");
        assert_eq!(
            path_from_uri("file:///a%20b/c.ts").as_deref(),
            Some(Path::new("/a b/c.ts"))
        );
    }

    #[test]
    fn a_windows_uri_drops_the_slash_before_the_drive_letter() {
        assert_eq!(
            path_from_uri("file:///C:/project/a.ts").as_deref(),
            Some(Path::new("C:/project/a.ts"))
        );
    }

    #[test]
    fn a_malformed_escape_is_left_alone_rather_than_dropped() {
        // Better a path that fails to match than a path silently missing characters.
        assert_eq!(
            path_from_uri("file:///a%zz/b.ts").as_deref(),
            Some(Path::new("/a%zz/b.ts"))
        );
    }

    #[test]
    fn a_non_file_uri_is_refused() {
        assert!(path_from_uri("untitled:Untitled-1").is_none());
        assert!(path_from_uri("https://example.com/a.ts").is_none());
    }

    #[test]
    fn capabilities_announce_open_and_save() {
        let announced = capabilities();
        let sync = &announced["capabilities"]["textDocumentSync"];
        assert_eq!(sync["openClose"], true);
        assert!(sync["save"].is_object());
        assert_eq!(announced["serverInfo"]["name"], "lanekeep");
    }
}
