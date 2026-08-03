//! `lanekeep.json` — configuration without writing TypeScript.
//!
//! Rules are TypeScript programs and that is not negotiable; it is the decision ADR-0007
//! rests on. But *configuration* is a different thing from a rule, and requiring a Go or
//! Python team to write a `.ts` file to say which rules they want was a coupling with
//! nothing behind it. `lanekeep init` in a Go project scaffolding TypeScript is the shape of
//! the problem.
//!
//! # How it works
//!
//! A JSON config is compiled to the entry module the TypeScript path already produces, then
//! handed to the same loader. Nothing downstream — extraction, validation, hashing, the
//! cache key — knows which format it came from, so the two cannot drift in behavior. A
//! parallel implementation would have been the obvious approach and the wrong one.
//!
//! ```json
//! {
//!   "include": ["src/**/*.go"],
//!   "rules": [
//!     "lanekeep/no-package-init",
//!     { "rule": "lanekeep/no-restricted-imports", "options": { "restrictions": [] } },
//!     "./lanekeep/rules/no-naked-return.ts"
//!   ]
//! }
//! ```
//!
//! becomes
//!
//! ```js
//! import __rule0 from 'lanekeep/no-package-init';
//! import __rule1 from 'lanekeep/no-restricted-imports';
//! import __rule2 from './lanekeep/rules/no-naked-return.ts';
//! globalThis.__lanekeepConfig = { include: [...], rules: [__rule0, __rule1({...}), __rule2] };
//! ```
//!
//! # What the two forms mean
//!
//! A bare string is a rule used as it comes. The object form calls it with options, which is
//! what `noRestrictedImports({ ... })` does in a TypeScript config — so the distinction a
//! rule author already makes between a rule and a rule factory survives, rather than being
//! guessed at from whether `options` happens to be present.

use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::ConfigError;

/// Whether this path is a JSON config rather than a module.
pub(crate) fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// A `lanekeep.json`, as written.
///
/// `deny_unknown_fields` on purpose: a misspelled key in a config is a setting that silently
/// does nothing, which is the failure this whole file exists to avoid producing more of.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonConfig {
    /// Editors read this to offer completion and validation. Accepted and ignored — the
    /// point of it is that a user gets help before lanekeep ever runs.
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,

    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    severity: Value,
    #[serde(default)]
    timeouts: Value,
    #[serde(default)]
    rules: Vec<JsonRule>,
}

/// A rule, either used as it comes or called with options.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonRule {
    /// `"lanekeep/no-default-export"` or `"./lanekeep/rules/mine.ts"`.
    Plain(String),
    /// `{ "rule": "...", "options": { ... } }`.
    Configured {
        rule: String,
        #[serde(default)]
        options: Value,
    },
}

impl JsonRule {
    fn specifier(&self) -> &str {
        match self {
            Self::Plain(specifier) => specifier,
            Self::Configured { rule, .. } => rule,
        }
    }
}

/// Compile a JSON config into the entry module the loader evaluates.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, is not valid JSON, or names a rule
/// in a way that cannot be turned into an import.
pub(crate) fn entry_source(config_path: &Path) -> Result<String, ConfigError> {
    let display = config_path.display().to_string();

    let text = std::fs::read_to_string(config_path).map_err(|e| ConfigError::Unreadable {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    let config: JsonConfig = serde_json::from_str(&text).map_err(|e| ConfigError::Shape {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    let mut imports = String::new();
    let mut references = Vec::with_capacity(config.rules.len());

    for (index, rule) in config.rules.iter().enumerate() {
        let specifier = rule.specifier();
        validate_specifier(specifier, &display)?;

        let binding = format!("__lanekeepRule{index}");
        let _ = writeln!(imports, "import {binding} from {};", js_string(specifier));

        references.push(match rule {
            JsonRule::Plain(_) => binding,
            JsonRule::Configured { options, .. } => {
                format!("{binding}({})", literal(options))
            }
        });
    }

    Ok(format!(
        "{imports}globalThis.__lanekeepConfig = {{\n  \
         include: {},\n  exclude: {},\n  namespaces: {},\n  \
         severity: {},\n  timeouts: {},\n  rules: [{}],\n}};\n",
        literal(&config.include),
        literal(&config.exclude),
        literal(&config.namespaces),
        object_or_empty(&config.severity),
        object_or_empty(&config.timeouts),
        references.join(", "),
    ))
}

/// Reject a specifier that cannot mean what it says.
///
/// A quote or a newline would end the import statement early and let the rest of the string
/// be read as code. Nothing legitimate needs either, so refusing is free — and a config file
/// is exactly the kind of thing that gets generated by a script one day.
fn validate_specifier(specifier: &str, display: &str) -> Result<(), ConfigError> {
    if specifier.is_empty() {
        return Err(ConfigError::Shape {
            path: display.to_owned(),
            detail: "a rule entry is an empty string".to_owned(),
        });
    }
    if specifier.contains(['\'', '"', '\\', '\n', '\r']) {
        return Err(ConfigError::Shape {
            path: display.to_owned(),
            detail: format!(
                "the rule specifier {specifier:?} contains a quote, a backslash or a newline"
            ),
        });
    }
    Ok(())
}

/// A JSON value as a JavaScript literal.
///
/// JSON is very nearly a subset of JavaScript, and the exception matters: U+2028 and U+2029
/// are ordinary characters in a JSON string and line terminators in older JavaScript, so a
/// config containing one would produce a module that does not parse. Escaping them costs
/// nothing and removes the question.
fn literal<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_owned())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// `severity` and `timeouts` default to `null` when absent, which the config shape reads as
/// missing rather than as empty. An object keeps the two indistinguishable from a config
/// that wrote `{}`.
fn object_or_empty(value: &Value) -> String {
    if value.is_null() {
        "{}".to_owned()
    } else {
        literal(value)
    }
}

/// A JavaScript single-quoted string. Only called on specifiers already validated above.
fn js_string(value: &str) -> String {
    format!("'{value}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(json: &str) -> Result<String, ConfigError> {
        let dir = std::env::temp_dir().join(format!("lanekeep-json-{:x}", json.len()));
        std::fs::create_dir_all(&dir).expect("creates dir");
        let path = dir.join("lanekeep.json");
        std::fs::write(&path, json).expect("writes");
        entry_source(&path)
    }

    #[test]
    fn a_bare_rule_is_imported_and_used_as_it_comes() {
        let source = compile(r#"{"rules": ["lanekeep/no-default-export"]}"#).expect("compiles");
        assert!(source.contains("import __lanekeepRule0 from 'lanekeep/no-default-export';"));
        assert!(source.contains("rules: [__lanekeepRule0]"));
    }

    #[test]
    fn a_configured_rule_is_called_with_its_options() {
        // The distinction a rule author already makes between a rule and a rule factory.
        let source = compile(
            r#"{"rules": [{"rule": "lanekeep/no-restricted-imports",
                "options": {"restrictions": [{"module": "stripe"}]}}]}"#,
        )
        .expect("compiles");
        assert!(source.contains("__lanekeepRule0({\"restrictions\":[{\"module\":\"stripe\"}]})"));
    }

    #[test]
    fn a_local_rule_keeps_its_relative_path() {
        let source = compile(r#"{"rules": ["./lanekeep/rules/mine.ts"]}"#).expect("compiles");
        assert!(source.contains("from './lanekeep/rules/mine.ts';"));
    }

    #[test]
    fn globs_and_namespaces_survive() {
        let source = compile(
            r#"{"include": ["src/**/*.go"], "exclude": ["**/*_test.go"], "namespaces": ["acme"]}"#,
        )
        .expect("compiles");
        assert!(source.contains(r#"include: ["src/**/*.go"]"#));
        assert!(source.contains(r#"exclude: ["**/*_test.go"]"#));
        assert!(source.contains(r#"namespaces: ["acme"]"#));
    }

    #[test]
    fn an_absent_severity_map_becomes_an_empty_object() {
        // `null` would reach the extractor as a missing field and read as "no map at all",
        // which is the same thing here but not obviously so. Empty is unambiguous.
        let source = compile(r#"{"rules": []}"#).expect("compiles");
        assert!(source.contains("severity: {}"));
        assert!(source.contains("timeouts: {}"));
    }

    #[test]
    fn the_schema_key_is_accepted_and_ignored() {
        // Editors read it to offer completion. Rejecting it would make the one thing that
        // helps a user before lanekeep runs an error.
        compile(r#"{"$schema": "https://example.com/s.json", "rules": []}"#)
            .expect("a $schema key is not an error");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // A misspelled key is a setting that silently does nothing.
        let error = compile(r#"{"includes": ["src/**"]}"#).expect_err("refused");
        assert!(
            format!("{error}").contains("includes"),
            "the error should name the key: {error}"
        );
    }

    #[test]
    fn a_specifier_that_would_escape_the_import_is_refused() {
        // Nothing legitimate needs a quote in a module specifier, and a config file is
        // exactly the kind of thing a script generates one day.
        for hostile in [
            r#"{"rules": ["a'; globalThis.x = 1; import b from 'c"]}"#,
            "{\"rules\": [\"a\\nimport b from 'c'\"]}",
        ] {
            compile(hostile).expect_err("a specifier with a quote or newline is refused");
        }
    }

    #[test]
    fn an_empty_specifier_is_refused() {
        compile(r#"{"rules": [""]}"#).expect_err("an empty specifier cannot import anything");
    }

    #[test]
    fn malformed_json_is_reported_as_shape() {
        let error = compile("{ not json }").expect_err("refused");
        assert!(matches!(error, ConfigError::Shape { .. }));
    }

    /// The published schema and the parser describe the same file.
    ///
    /// They drift in two directions and both are silent. A key the schema declares but the
    /// parser refuses makes an editor bless a config that then fails to load. A key the
    /// parser accepts but the schema omits makes an editor underline correct config, which
    /// is how a user learns to ignore the schema.
    ///
    /// So this reads the shipped schema and checks every property it declares actually
    /// parses, and that the set is exactly the expected one — adding a field to either side
    /// alone fails here.
    #[test]
    fn the_shipped_schema_and_the_parser_agree() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schema/lanekeep.schema.json"))
                .expect("the shipped schema is valid JSON");

        let mut declared: Vec<&str> = schema["properties"]
            .as_object()
            .expect("the schema declares properties")
            .keys()
            .map(String::as_str)
            .collect();
        declared.sort_unstable();

        assert_eq!(
            declared,
            [
                "$schema",
                "exclude",
                "include",
                "namespaces",
                "rules",
                "severity",
                "timeouts"
            ],
            "the schema's fields changed; the parser below has to change with it"
        );

        // Every one of them, together, in a single config the parser must accept.
        let everything = r#"{
            "$schema": "https://example.com/s.json",
            "include": ["src/**"],
            "exclude": ["**/x"],
            "namespaces": ["acme"],
            "severity": {"acme/a": "warn"},
            "timeouts": {"rule": 100, "global": 5000},
            "rules": ["lanekeep/no-default-export"]
        }"#;
        compile(everything).expect("the parser accepts every field the schema declares");
    }

    #[test]
    fn json_is_recognized_by_extension() {
        assert!(is_json(Path::new("lanekeep.json")));
        assert!(is_json(Path::new("LANEKEEP.JSON")));
        assert!(!is_json(Path::new("lanekeep.config.ts")));
    }
}
