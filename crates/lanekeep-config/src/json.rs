//! `lanekeep.json` — configuration without writing TypeScript.
//!
//! Rules are programs and that is not negotiable; it is the decision ADR-0007 rests on. But
//! *configuration* is a different thing from a rule, and requiring a Go or Python team to
//! write a `.ts` file to say which rules they want was a coupling with nothing behind it.
//! `lanekeep init` in a Go project scaffolding TypeScript is the shape of the problem.
//!
//! # How it works
//!
//! This file parses, validates and resolves a JSON config in Rust, with no JavaScript
//! evaluated at any point. A rule reference — a bare string or a `{ "rule", "options" }`
//! object — resolves to a [`RuleReference`] naming a built-in, a compiled component or a
//! rule module, with its `options` carried alongside as data.
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
//! # What the two forms mean
//!
//! A bare string is a rule used as it comes. The object form configures it with options,
//! which is what `noRestrictedImports({ ... })` does in a TypeScript config — so the
//! distinction a rule author already makes between a rule and a rule factory survives,
//! rather than being guessed at from whether `options` happens to be present.
//!
//! # It used to be compiled into JavaScript, and why it no longer is
//!
//! Until this file was un-coupled, a JSON config was compiled into the entry module the
//! TypeScript path produces and handed to the same loader, so that nothing downstream knew
//! which format it came from and "the two cannot drift in behavior." That was a deliberate
//! design and a good one; it is also the mechanism that made `lanekeep.json` depend on a
//! JavaScript sandbox, which is why removing QuickJS would have broken *config loading*
//! rather than only rule execution.
//!
//! What replaces it is convergence rather than a shared mechanism: both formats still meet
//! at exactly one place — `crate::build` — which validates, hashes and constructs the
//! `Config`. Nothing about a rule's identity, severity, card, query or budget is decided
//! twice. See `lib.rs`'s note above `entry_source` for what the change costs and what now
//! holds the two paths together.
//!
//! One thing does still cross into JavaScript on this path, and it is rule *execution*, not
//! configuration: a reference naming a TypeScript rule is rendered into a rules-only entry
//! module by [`rules_module`], because a TypeScript rule's `id`, `query` and `card` live
//! inside its own `defineRule` call and nothing but evaluating it can read them. A component
//! is the other way round — it answers `metadata` itself, so it contributes no import and
//! nothing for the sandbox to evaluate.
//!
//! **A config naming only components still evaluates an entry module, and that is a residue
//! rather than a requirement.** `crate::load` always evaluates and always runs `EXTRACT`, so a
//! components-only config produces `globalThis.__lanekeepConfig = { rules: [null] }` and gets
//! a list of nulls back. Nothing is read from it. Skipping the sandbox when no reference names
//! a module is a real simplification and is not made here, because the same entry module is
//! what every worker evaluates and that path has to agree with this one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use lanekeep_core::files::normalize;
use serde::Deserialize;
use serde_json::Value;

use crate::ConfigError;

/// Resolves a built-in rule name to its embedded component.
///
/// Structurally the same type the rules root carries and `crate::load` hands in — a `fn` alias
/// is transparent, so the two are one type and a change to either is a compile error at the
/// call site rather than a drift.
///
/// **Declared here rather than imported, and the reason is the point of this file.** Resolving a
/// `lanekeep.json` reaches no sandbox, and `the_json_path_names_nothing_from_the_sandbox_crate`
/// enforces that by refusing the sandbox crate's name anywhere in this source. A lookup function
/// is not a sandbox and importing the alias would not make it one — but the check reads names
/// rather than intent, deliberately, because the alternative is a check that has to be argued
/// with every time it fires. This file does not need the import, so it does not take it.
pub(crate) type BuiltinComponent = fn(&str) -> Option<(&'static [u8], u32)>;

/// The prefix a built-in rule reference carries, as in `lanekeep/no-package-init`.
const BUILTIN_PREFIX: &str = "lanekeep/";

/// The extension marking a reference as a compiled rule component.
const COMPONENT_EXTENSION: &str = "wasm";

/// Whether this path is a JSON config rather than a module.
pub(crate) fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// What a rule reference in a `lanekeep.json` names.
///
/// Deciding this in Rust is the whole of the un-coupling: a reference used to become an
/// `import` statement whose meaning only the module loader knew, and is now a value the
/// rest of the crate can read without evaluating anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleReference {
    /// A rule shipped with lanekeep, authored in TypeScript. Carries the name after the
    /// prefix, so `"lanekeep/no-package-init"` is `Builtin("no-package-init")`.
    ///
    /// Which bytes that name resolves to is the loader's business; the name is what a config
    /// wrote, and it is decided here.
    Builtin(String),

    /// A rule shipped with lanekeep, compiled to a component.
    ///
    /// **The distinction is not one a config writes.** Both spellings are `lanekeep/<name>`,
    /// and which of the two a name is depends only on how that rule happens to be authored in
    /// the build the user is running. A rule migrating from TypeScript to Rust must not require
    /// anybody to edit their config, which is the whole point of resolving the prefix here
    /// rather than making the format carry the answer.
    ///
    /// Its own variant rather than a [`RuleReference::Component`] holding a synthetic path,
    /// because a built-in has no path: its bytes are embedded in the binary, so there is
    /// nothing to confine, nothing to read and nothing a project file could shadow.
    BuiltinComponent(String),

    /// A compiled rule component on disk, as in `"./rules/no-package-init.wasm"`.
    ///
    /// The path is the reference resolved against the rules root — the same anchor a
    /// relative module specifier resolves against, since the synthetic entry module sits
    /// there.
    ///
    /// Resolved rather than merely recognized: `crate::describe_components` loads these bytes
    /// at config load and asks the component what it is, because a `.wasm` carries its own
    /// `id`, `query`, `card` and gates and there is no config syntax for any of them.
    Component(PathBuf),

    /// A rule module on disk, as in `"./lanekeep/rules/mine.ts"`.
    ///
    /// Carries the specifier as written rather than a path, because the extension is
    /// optional — `./rule` finds `rule.ts` — and reproducing the loader's search here would
    /// be a second implementation of it.
    Module(String),
}

impl RuleReference {
    /// Whether this reference's handlers live in a component rather than in JavaScript.
    ///
    /// One question asked in three places — the entry module's placeholder, the early return in
    /// `crate::describe_components`, and the description loop — so that adding a third way for a
    /// component to be named cannot leave one of them behind. A `matches!` at each site is what
    /// let `BuiltinComponent` be added and forgotten, and every symptom of forgetting is silent:
    /// a placeholder not emitted shifts every later rule's handler by one, and a reference not
    /// described becomes a rule with no `check`.
    #[must_use]
    pub const fn is_component(&self) -> bool {
        matches!(self, Self::Component(_) | Self::BuiltinComponent(_))
    }
}

/// A rule reference, resolved, with the options it was configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRule {
    /// The reference exactly as the config wrote it.
    pub specifier: String,
    /// What it names.
    pub reference: RuleReference,
    /// The options it was configured with, as data.
    ///
    /// `None` is the bare-string form — a rule used as it comes. `Some` is the object form,
    /// including `{ "rule": "x" }` with no `options` key, which configures it with `null`.
    /// The distinction is the one a rule author already makes between a rule and a rule
    /// factory, and it is not inferred from whether a value happens to be present.
    ///
    /// A component cannot close over a host-supplied value the way a JavaScript factory does,
    /// so `crates/lanekeep-wasm/wit/world.wit` declares `configure(options-json)` for exactly
    /// that reason — a call made once after instantiation, taking this field serialized as
    /// JSON (`null` for the bare-string form). `crate::describe_components` serializes it once
    /// and records it with the rule, so the bytes every worker's `configure` is handed are the
    /// same bytes.
    pub options: Option<Value>,
}

/// A `lanekeep.json`, parsed and resolved.
pub(crate) struct Parsed {
    /// Everything that is configuration data, in the shape [`crate::build`] consumes.
    ///
    /// The same struct the TypeScript path fills in from `JSON.stringify`, with `rules`
    /// left empty — those arrive from extraction, and are the one field on this path that
    /// still comes from the sandbox.
    pub(crate) config: crate::RawConfig,
    /// The rule references, resolved.
    pub(crate) rules: Vec<ResolvedRule>,
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
    severity: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    timeouts: crate::RawTimeouts,
    #[serde(default)]
    suppressions: crate::RawSuppressions,
    #[serde(default)]
    rules: Vec<JsonRule>,
}

/// A rule, either used as it comes or configured with options.
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

    fn options(&self) -> Option<Value> {
        match self {
            Self::Plain(_) => None,
            Self::Configured { options, .. } => Some(options.clone()),
        }
    }
}

/// Read a JSON config and resolve every rule reference, evaluating nothing.
///
/// `rules_root` anchors a relative reference, because that is where the synthetic entry
/// module sits and therefore what a relative specifier has always resolved against.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, is not valid JSON, or names a rule
/// in a way that cannot mean anything.
pub(crate) fn parse(
    config_path: &Path,
    rules_root: &Path,
    components: BuiltinComponent,
) -> Result<Parsed, ConfigError> {
    let display = config_path.display().to_string();

    let text = std::fs::read_to_string(config_path).map_err(|e| ConfigError::Unreadable {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    let config: JsonConfig = serde_json::from_str(&text).map_err(|e| ConfigError::Shape {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    let mut rules = Vec::with_capacity(config.rules.len());
    for rule in &config.rules {
        let specifier = rule.specifier();
        validate_specifier(specifier, &display)?;
        rules.push(ResolvedRule {
            specifier: specifier.to_owned(),
            reference: classify(specifier, rules_root, components),
            options: rule.options(),
        });
    }

    Ok(Parsed {
        // Every field named, and no `..Default::default()`. This is the load-bearing line of
        // the whole un-coupling: the two config formats no longer share a mechanism, so the
        // thing that has to be prevented is one of them quietly not carrying a setting. An
        // exhaustive literal against the *shared* struct makes that a compile error — adding
        // `presets` to `RawConfig` fails here with `error[E0063]: missing field 'presets'`,
        // naming this line — where the arrangement this replaced had no equivalent: the same
        // omission from the old entry module's `format!` string compiled fine and produced a
        // config silently missing a setting. Do not "tidy" this into a struct-update.
        config: crate::RawConfig {
            include: config.include,
            exclude: config.exclude,
            namespaces: config.namespaces,
            severity: config.severity,
            timeouts: config.timeouts,
            suppressions: config.suppressions,
            rules: Vec::new(),
        },
        rules,
    })
}

/// Decide what a validated specifier names.
///
/// Built-ins are recognized before anything else, exactly as the module loader does, so a
/// file on disk cannot shadow one. A `.wasm` extension is what distinguishes a compiled
/// component from a source module; nothing else is ambiguous, because a rule module's
/// extension is optional and a component's never is — bytes are not searched for by guessing
/// suffixes.
///
/// # Whether a built-in is a component is asked, not spelled
///
/// `components` is the same lookup the rules root answers module resolution from, so one value
/// decides what `lanekeep/<name>` means everywhere. Splitting it would let a name be a module
/// here and a component there — and the failure would be a rule that loads and never runs,
/// which reads exactly like a clean codebase.
///
/// A name the lookup does not know stays a [`RuleReference::Builtin`] rather than becoming an
/// error here: an unknown built-in is refused by the loader, with the message that already
/// names the specifier and says no such rule ships. Two refusals for one mistake would differ
/// in wording by the format the user happened to write.
fn classify(specifier: &str, rules_root: &Path, components: BuiltinComponent) -> RuleReference {
    if let Some(name) = specifier.strip_prefix(BUILTIN_PREFIX) {
        return if components(name).is_some() {
            RuleReference::BuiltinComponent(name.to_owned())
        } else {
            RuleReference::Builtin(name.to_owned())
        };
    }
    let path = Path::new(specifier);
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(COMPONENT_EXTENSION))
    {
        return RuleReference::Component(normalize(&rules_root.join(path)));
    }
    RuleReference::Module(specifier.to_owned())
}

/// Compile the resolved rules into the entry module the loader evaluates.
///
/// Only the rules: `include`, `exclude`, `namespaces`, `severity` and `timeouts` are read in
/// Rust by [`parse`] and never become JavaScript. What is left here is the one thing a
/// sandbox is still required for — reading a TypeScript rule's own declaration.
///
/// # A component holds its place in the array and contributes no JavaScript
///
/// A component is resolved in Rust: its `id`, `query` and card come from its own `metadata`
/// export, so there is nothing here to import and nothing for the sandbox to evaluate. What
/// it emits instead is a literal `null` in the array, and that is load-bearing rather than
/// tidy.
///
/// `RuleSpec::index` is how the engine reaches a *TypeScript* handler — it is spelled
/// `globalThis.__lanekeepConfig.rules[index].check(...)` — and it is also the position
/// `lanekeep-config` builds every rule at. Skipping a component would make those two numbers
/// disagree the moment a config mixes the kinds: rule 3 in the config would be rule 2 in the
/// array, and every rule after a component would dispatch to its neighbor. That failure is
/// silent — a rule object is a rule object, the call succeeds, and the violations are simply
/// attributed to the wrong rule. A placeholder keeps one numbering for both, so there is no
/// mapping to get wrong.
///
/// Nothing ever reads the placeholder: a component-backed rule dispatches on
/// `RuleSpec::component`, not through this array. The one thing that touches it is `EXTRACT`,
/// which is written with `?.` throughout and yields a rule with no `id` and no `check` — which
/// is exactly what a component's entry in the extracted array should look like, since its real
/// answers come from somewhere else entirely.
pub(crate) fn rules_module(rules: &[ResolvedRule]) -> String {
    let mut imports = String::new();
    let mut references = Vec::with_capacity(rules.len());

    for (index, rule) in rules.iter().enumerate() {
        if rule.reference.is_component() {
            references.push("null".to_owned());
            continue;
        }

        let binding = format!("__lanekeepRule{index}");
        let _ = writeln!(
            imports,
            "import {binding} from {};",
            js_string(&rule.specifier)
        );

        references.push(match &rule.options {
            None => binding,
            Some(options) => {
                let specifier = js_string(&rule.specifier);
                format!(
                    "(function() {{ var __r = {binding}; var __o = {literal}; \
                     if (typeof __r === 'function') return __r(__o); \
                     if (__o !== null && __o !== undefined) \
                     throw new Error({specifier} + ' takes no options — \
                     it exports a rule object, not a factory'); \
                     return __r; }})()",
                    binding = binding,
                    literal = literal(options),
                    specifier = specifier,
                )
            }
        });
    }

    format!(
        "{imports}globalThis.__lanekeepConfig = {{ rules: [{}] }};\n",
        references.join(", "),
    )
}

/// Reject a specifier that cannot mean what it says.
///
/// A quote or a newline would end the import statement early and let the rest of the string
/// be read as code. Nothing legitimate needs either, so refusing is free — and a config file
/// is exactly the kind of thing that gets generated by a script one day.
///
/// Applied to every reference rather than only to the ones still rendered into JavaScript.
/// A path carrying a quote is not a path anyone means, and a check that holds for some
/// reference kinds and not others is a check whose coverage depends on a classification made
/// somewhere else.
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
///
/// Also what the options blob is hashed as, where the escaping is irrelevant and the
/// canonical ordering is not: `serde_json::Map` is a `BTreeMap` here, so two configs writing
/// the same option keys in a different order serialize identically and hash the same.
pub(crate) fn literal<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_owned())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// A JavaScript single-quoted string. Only called on specifiers already validated above.
fn js_string(value: &str) -> String {
    format!("'{value}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a fixture config is written: one directory per caller, named by the caller.
    ///
    /// **Two derived names have already raced here, and the second looked like a fix.** The
    /// first keyed the directory on the config's *length*, so two thirty-eight-byte configs
    /// shared one file and each test read whichever had been written last. Keying on a hash
    /// of the *content* was the obvious repair and is still wrong: two tests can legitimately
    /// use the identical config — `a_component_reference_resolves_to_a_path` and
    /// `a_component_reference_imports_nothing` both write
    /// `{"rules": ["./rules/mine.wasm"]}` — and `std::fs::write` truncates before it writes,
    /// so the sibling thread reads an empty file and fails with `EOF while parsing a value at
    /// line 1 column 0`. Measured five failures in eighty runs of
    /// `cargo test -p lanekeep-config`. Same bytes is not the same as no race; truncate-then-
    /// write is not atomic.
    ///
    /// An explicit name is the only version with no derivation to be clever about. Two tests
    /// passing the same name is a visible duplicate rather than a scheduling-dependent one.
    fn fixture_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lanekeep-json-{name}"))
    }

    /// No built-in ships as a component, so `lanekeep/<name>` is a module.
    ///
    /// The default for these tests, so that the ones about imports and options say what they
    /// always said regardless of which real rules have migrated.
    fn no_components(_name: &str) -> Option<(&'static [u8], u32)> {
        None
    }

    /// One built-in ships as a component, named for what it is rather than after a real rule.
    ///
    /// A stub rather than `lanekeep_rules::component`, on the same terms as the loader's own
    /// stub: which rules ship is not what these tests are about, and pinning them to the real
    /// table would make a future migration edit assertions that have nothing to do with it.
    fn one_component(name: &str) -> Option<(&'static [u8], u32)> {
        match name {
            "compiled" => Some((b"\0asm\x01\x00\x00\x00", 0)),
            _ => None,
        }
    }

    fn parse_config(name: &str, json: &str) -> Result<Parsed, ConfigError> {
        parse_config_with(name, json, no_components)
    }

    fn parse_config_with(
        name: &str,
        json: &str,
        components: BuiltinComponent,
    ) -> Result<Parsed, ConfigError> {
        let dir = fixture_dir(name);
        std::fs::create_dir_all(&dir).expect("creates dir");
        let path = dir.join("lanekeep.json");
        std::fs::write(&path, json).expect("writes");
        parse(&path, &dir, components)
    }

    fn compile(name: &str, json: &str) -> Result<String, ConfigError> {
        let parsed = parse_config(name, json)?;
        Ok(rules_module(&parsed.rules))
    }

    #[test]
    fn a_bare_rule_is_imported_and_used_as_it_comes() {
        let source =
            compile("bare-rule", r#"{"rules": ["lanekeep/no-default-export"]}"#).expect("compiles");
        assert!(source.contains("import __lanekeepRule0 from 'lanekeep/no-default-export';"));
        assert!(source.contains("rules: [__lanekeepRule0]"));
    }

    #[test]
    fn a_configured_rule_is_called_with_its_options() {
        // The distinction a rule author already makes between a rule and a rule factory.
        // The entry module wraps the call in a guard that checks `typeof __r === 'function'`
        // so a non-factory rule given options throws a descriptive error rather than
        // `not a function` from QuickJS.
        let source = compile(
            "configured-rule",
            r#"{"rules": [{"rule": "lanekeep/no-restricted-imports",
                "options": {"restrictions": [{"module": "stripe"}]}}]}"#,
        )
        .expect("compiles");
        assert!(
            source.contains(r#"{"restrictions":[{"module":"stripe"}]}"#),
            "the options literal must appear in the entry module:\n{source}"
        );
        assert!(
            source.contains("__lanekeepRule0"),
            "the rule binding must be referenced:\n{source}"
        );
    }

    #[test]
    fn a_local_rule_keeps_its_relative_path() {
        let source =
            compile("local-rule", r#"{"rules": ["./lanekeep/rules/mine.ts"]}"#).expect("compiles");
        assert!(source.contains("from './lanekeep/rules/mine.ts';"));
    }

    #[test]
    fn globs_and_namespaces_survive() {
        let parsed = parse_config(
            "globs-and-namespaces",
            r#"{"include": ["src/**/*.go"], "exclude": ["**/*_test.go"], "namespaces": ["acme"]}"#,
        )
        .expect("parses");
        assert_eq!(parsed.config.include, ["src/**/*.go"]);
        assert_eq!(parsed.config.exclude, ["**/*_test.go"]);
        assert_eq!(parsed.config.namespaces, ["acme"]);
    }

    /// The data half never becomes JavaScript, which is what un-coupling this path meant.
    ///
    /// Asserted on the generated module rather than on a call graph, because the module is
    /// the whole of what the sandbox is asked to evaluate: if a value is not in it, no
    /// amount of evaluation can reach it.
    #[test]
    fn no_configuration_data_reaches_the_entry_module() {
        let source = compile(
            "no-data-in-module",
            r#"{"include": ["src/**/*.go"], "exclude": ["**/*_test.go"],
                "namespaces": ["acme"], "severity": {"acme/a": "warn"},
                "timeouts": {"rule": 100, "global": 5000},
                "suppressions": {"requireExpiry": true, "maxExpiryDays": 30,
                                 "forbidFileScope": true},
                "rules": ["lanekeep/no-default-export"]}"#,
        )
        .expect("compiles");

        for absent in [
            "src/**/*.go",
            "*_test.go",
            "acme",
            "warn",
            "severity",
            "timeouts",
            "suppressions",
            "requireExpiry",
            "maxExpiryDays",
            "forbidFileScope",
            "include",
            "exclude",
        ] {
            assert!(
                !source.contains(absent),
                "`{absent}` should not reach the sandbox: {source}"
            );
        }
    }

    #[test]
    fn severity_and_timeouts_are_read_in_rust() {
        let parsed = parse_config(
            "severity-and-timeouts",
            r#"{"severity": {"acme/a": "warn"}, "timeouts": {"rule": 100, "global": 5000}}"#,
        )
        .expect("parses");
        assert_eq!(
            parsed.config.severity.get("acme/a").map(String::as_str),
            Some("warn")
        );
        assert_eq!(parsed.config.timeouts.rule, Some(100));
        assert_eq!(parsed.config.timeouts.global, Some(5000));
    }

    #[test]
    fn suppressions_are_read_in_rust() {
        let parsed = parse_config(
            "suppressions-in-rust",
            r#"{"suppressions": {"requireExpiry": true, "maxExpiryDays": 30, "forbidFileScope": true}}"#,
        )
        .expect("parses");
        assert!(parsed.config.suppressions.require_expiry);
        assert_eq!(parsed.config.suppressions.max_expiry_days, Some(30));
        assert!(parsed.config.suppressions.forbid_file_scope);
    }

    #[test]
    fn a_builtin_resolves_to_its_name() {
        let parsed =
            parse_config("builtin", r#"{"rules": ["lanekeep/no-package-init"]}"#).expect("parses");
        assert_eq!(
            parsed.rules[0].reference,
            RuleReference::Builtin("no-package-init".to_owned())
        );
        assert_eq!(parsed.rules[0].options, None);
    }

    #[test]
    fn a_module_reference_keeps_its_specifier() {
        let parsed =
            parse_config("module-ref", r#"{"rules": ["./rules/mine.ts"]}"#).expect("parses");
        assert_eq!(
            parsed.rules[0].reference,
            RuleReference::Module("./rules/mine.ts".to_owned())
        );
    }

    /// A component is recognized by its extension and resolved against the rules root.
    #[test]
    fn a_component_reference_resolves_to_a_path() {
        let parsed =
            parse_config("component-path", r#"{"rules": ["./rules/mine.wasm"]}"#).expect("parses");
        assert_eq!(
            parsed.rules[0].reference,
            RuleReference::Component(
                fixture_dir("component-path")
                    .join("rules")
                    .join("mine.wasm")
            )
        );
    }

    /// A component is resolved in Rust, so it contributes no import and no rule object.
    #[test]
    fn a_component_reference_imports_nothing() {
        let source = compile(
            "component-placeholder",
            r#"{"rules": ["./rules/mine.wasm"]}"#,
        )
        .expect("a component is resolved without the sandbox");
        assert_eq!(source, "globalThis.__lanekeepConfig = { rules: [null] };\n");
    }

    /// And the reason its place is held rather than closed up.
    #[test]
    fn a_rule_after_a_component_keeps_its_position_in_the_array() {
        // `RuleSpec::index` is spelled `__lanekeepConfig.rules[index].check(...)` by the
        // engine, and it is also the position `build` numbers every rule at. Skipping the
        // component would make rule 2 of the config rule 1 of the array, so every rule after a
        // component would dispatch to its neighbor — a call that succeeds, and violations
        // attributed to the wrong rule.
        let source = compile(
            "component-numbering",
            r#"{"rules": ["./rules/mine.wasm", "lanekeep/no-default-export", "./mine.ts"]}"#,
        )
        .expect("compiles");

        assert!(
            source.contains("import __lanekeepRule1 from 'lanekeep/no-default-export';"),
            "{source}"
        );
        assert!(
            source.contains("rules: [null, __lanekeepRule1, __lanekeepRule2]"),
            "{source}"
        );
    }

    /// A built-in that ships as a component is one, and the config says nothing about it.
    #[test]
    fn a_built_in_with_a_component_resolves_to_one() {
        let parsed = parse_config_with(
            "builtin-component",
            r#"{"rules": ["lanekeep/compiled"]}"#,
            one_component,
        )
        .expect("parses");

        assert_eq!(
            parsed.rules[0].reference,
            RuleReference::BuiltinComponent("compiled".to_owned())
        );
        // The specifier is unchanged, which is the property that matters to a user: a rule
        // migrating from TypeScript to Rust must not need anybody to edit a config.
        assert_eq!(parsed.rules[0].specifier, "lanekeep/compiled");
    }

    /// The same name, in a build where that rule is still TypeScript.
    ///
    /// The pair is the point. One config, two builds, and the only difference is which table
    /// the rule is in — so an assertion on either alone would pass against a `classify` that
    /// ignored the lookup entirely.
    #[test]
    fn the_same_built_in_is_a_module_when_no_component_ships() {
        let parsed = parse_config_with(
            "builtin-still-typescript",
            r#"{"rules": ["lanekeep/compiled"]}"#,
            no_components,
        )
        .expect("parses");

        assert_eq!(
            parsed.rules[0].reference,
            RuleReference::Builtin("compiled".to_owned())
        );
    }

    /// A built-in component contributes no import, exactly as a `.wasm` path does.
    #[test]
    fn a_built_in_component_imports_nothing() {
        let source = {
            let parsed = parse_config_with(
                "builtin-component-placeholder",
                r#"{"rules": ["lanekeep/compiled"]}"#,
                one_component,
            )
            .expect("parses");
            rules_module(&parsed.rules)
        };
        assert_eq!(source, "globalThis.__lanekeepConfig = { rules: [null] };\n");
    }

    /// And holds its place, for the same reason a `.wasm` path does.
    ///
    /// Asserted separately from `a_rule_after_a_component_keeps_its_position_in_the_array`
    /// rather than trusted to it: the two reach the placeholder through different arms of
    /// `classify`, and a `matches!` that named only one of them would leave this arm shifting
    /// every later rule's handler by one — silently, since the call still succeeds.
    #[test]
    fn a_rule_after_a_built_in_component_keeps_its_position() {
        let parsed = parse_config_with(
            "builtin-component-numbering",
            r#"{"rules": ["lanekeep/compiled", "lanekeep/no-default-export", "./mine.ts"]}"#,
            one_component,
        )
        .expect("parses");
        let source = rules_module(&parsed.rules);

        assert!(
            source.contains("import __lanekeepRule1 from 'lanekeep/no-default-export';"),
            "{source}"
        );
        assert!(
            source.contains("rules: [null, __lanekeepRule1, __lanekeepRule2]"),
            "{source}"
        );
    }

    /// The object form's `options` are data on the way through, whatever the reference is.
    #[test]
    fn options_are_carried_as_data() {
        let parsed = parse_config(
            "options-as-data",
            r#"{"rules": [{"rule": "lanekeep/x", "options": {"limit": 3}}, {"rule": "lanekeep/y"}]}"#,
        )
        .expect("parses");
        assert_eq!(
            parsed.rules[0].options,
            Some(serde_json::json!({"limit": 3}))
        );
        // The object form with no `options` key configures with `null`, which is not the
        // same as the bare-string form and must not collapse into it.
        assert_eq!(parsed.rules[1].options, Some(Value::Null));
    }

    #[test]
    fn an_absent_severity_map_is_empty_rather_than_missing() {
        let parsed = parse_config("absent-severity", r#"{"rules": []}"#).expect("parses");
        assert!(parsed.config.severity.is_empty());
        assert_eq!(parsed.config.timeouts.rule, None);
        assert_eq!(parsed.config.timeouts.global, None);
    }

    #[test]
    fn the_schema_key_is_accepted_and_ignored() {
        // Editors read it to offer completion. Rejecting it would make the one thing that
        // helps a user before lanekeep runs an error.
        compile(
            "schema-key",
            r#"{"$schema": "https://example.com/s.json", "rules": []}"#,
        )
        .expect("a $schema key is not an error");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // A misspelled key is a setting that silently does nothing.
        let error = compile("unknown-key", r#"{"includes": ["src/**"]}"#).expect_err("refused");
        assert!(
            format!("{error}").contains("includes"),
            "the error should name the key: {error}"
        );
    }

    #[test]
    fn a_specifier_that_would_escape_the_import_is_refused() {
        // Nothing legitimate needs a quote in a module specifier, and a config file is
        // exactly the kind of thing a script generates one day.
        for (name, hostile) in [
            (
                "hostile-quote",
                r#"{"rules": ["a'; globalThis.x = 1; import b from 'c"]}"#,
            ),
            (
                "hostile-newline",
                "{\"rules\": [\"a\\nimport b from 'c'\"]}",
            ),
        ] {
            compile(name, hostile).expect_err("a specifier with a quote or newline is refused");
        }
    }

    #[test]
    fn an_empty_specifier_is_refused() {
        compile("empty-specifier", r#"{"rules": [""]}"#)
            .expect_err("an empty specifier cannot import anything");
    }

    #[test]
    fn malformed_json_is_reported_as_shape() {
        let error = compile("malformed", "{ not json }").expect_err("refused");
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
    /// parses, and that the set is exactly the expected one.
    ///
    /// **It catches one of those two directions, not both, and the docstring used to claim
    /// otherwise.** A field added to the schema alone changes `declared` and fails here. A
    /// field added to `JsonConfig` alone changes neither the schema file nor the list below,
    /// so it passes — the list is a hand-maintained third copy, and the check is really
    /// "schema versus list" rather than "schema versus parser". Closing it needs the struct's
    /// own field names, which `serde` does not expose and which nothing here can read without
    /// parsing this file's source. Left open deliberately, and stated, because a comment
    /// claiming a guarantee that is not there is worse than the gap: it is the reason nobody
    /// looks again.
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
                "suppressions",
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
            "suppressions": {"requireExpiry": true, "maxExpiryDays": 30,
                             "forbidFileScope": true},
            "rules": ["lanekeep/no-default-export"]
        }"#;
        compile("schema-agreement", everything)
            .expect("the parser accepts every field the schema declares");
    }

    #[test]
    fn json_is_recognized_by_extension() {
        assert!(is_json(Path::new("lanekeep.json")));
        assert!(is_json(Path::new("LANEKEEP.JSON")));
        assert!(!is_json(Path::new("lanekeep.config.ts")));
    }
}
