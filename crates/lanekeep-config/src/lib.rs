//! Configuration loading and canonicalized hashing for lanekeep.
//!
//! Loads `lanekeep.config.ts`, resolves the rule graph, and derives the hashes feeding the
//! cache key.
//!
//! # How the config is read
//!
//! The config is a TypeScript module, so reading it means running it. A synthetic entry
//! module imports the config's default export into a global, and a second evaluation hands
//! back `JSON.stringify` of the parts that are data.
//!
//! Going through JSON rather than reaching into engine values is deliberate. It keeps
//! every value crossing the boundary plainly serializable, it makes the whole extraction
//! one testable string, and it sidesteps threading engine lifetimes through this crate.
//!
//! The one thing it cannot carry is a function, and `check` is a function. So the
//! extraction separately records whether each rule has a callable `check` and `reduce`.
//! Without that, a rule whose handler was misspelled would load cleanly and silently never
//! fire — the worst failure this tool can have, because it looks exactly like passing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lanekeep_core::{Examples, RuleCard, RuleId, Severity};
use lanekeep_js::{Limits, RuleRoot, RunClock, Sandbox};
use serde::Deserialize;
use thiserror::Error;

/// A 32-byte content hash.
pub type Hash = [u8; 32];

/// Render a hash the way it appears in diagnostics and cache paths.
#[must_use]
pub fn hex(hash: &Hash) -> String {
    use std::fmt::Write as _;
    hash.iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The pre-parse gates a rule declares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Gates {
    /// Only files matching one of these globs are considered.
    pub path_matches: Vec<String>,
    /// Files matching any of these are skipped.
    pub path_not_matches: Vec<String>,
    /// Only files whose bytes contain all of these are parsed.
    pub file_contains: Vec<String>,
    /// Files whose bytes contain any of these are skipped.
    pub file_not_contains: Vec<String>,
}

impl Gates {
    /// Whether any gate is declared. A rule without gates parses every candidate file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.path_matches.is_empty()
            && self.path_not_matches.is_empty()
            && self.file_contains.is_empty()
            && self.file_not_contains.is_empty()
    }
}

/// A rule as the config declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSpec {
    /// Namespaced identifier.
    pub id: RuleId,
    /// Which language's grammar the query compiles against.
    pub language: String,
    /// Severity as the rule declares it, before config overrides.
    pub severity: Severity,
    /// The rule card.
    pub card: RuleCard,
    /// The tree-sitter query gating the handler.
    pub query: String,
    /// Pre-parse gates.
    pub gates: Gates,
    /// A per-invocation budget overriding the default.
    pub timeout: Option<Duration>,
    /// Whether the rule has a `reduce` phase.
    pub has_reduce: bool,
}

/// A loaded, validated configuration.
#[expect(
    clippy::struct_field_names,
    reason = "`ruleset_hash` and `config_hash` are the names docs/architecture.md §8.1 \
              gives these two cache-key inputs. Renaming them to satisfy the lint would \
              make the code and the specification disagree about the same thing, which \
              costs more than the repetition saves."
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Globs selecting files to check.
    pub include: Vec<String>,
    /// Globs excluding files from the selection.
    pub exclude: Vec<String>,
    /// Rules, in the order the config listed them.
    pub rules: Vec<RuleSpec>,
    /// Budgets, with defaults filled in.
    pub limits: Limits,
    /// Hash of every module in the rule import graph.
    pub ruleset_hash: Hash,
    /// Hash of the configuration values.
    pub config_hash: Hash,
}

/// Why a configuration could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// The config file does not exist or sits outside the project.
    #[error("cannot load config `{path}`: {detail}")]
    Unreadable {
        /// The path as given.
        path: String,
        /// What went wrong.
        detail: String,
    },

    /// The config module threw, failed to parse, or breached a limit.
    #[error("config `{path}` failed to evaluate\n{detail}")]
    Evaluation {
        /// The path as given.
        path: String,
        /// The sandbox's account of it.
        detail: String,
    },

    /// The config evaluated but is not shaped like a config.
    #[error("config `{path}` is not valid: {detail}")]
    Shape {
        /// The path as given.
        path: String,
        /// What is wrong.
        detail: String,
    },

    /// A rule in the config is not usable.
    #[error("rule {position} in `{path}` is not valid: {detail}")]
    Rule {
        /// One-based position in the `rules` array, so an unnamed rule can still be found.
        position: usize,
        /// The path as given.
        path: String,
        /// What is wrong.
        detail: String,
    },
}

/// The shape `JSON.stringify` hands back. Deliberately permissive — every field is checked
/// afterwards, so a malformed config produces a diagnostic naming the field rather than a
/// deserialization error naming a line of JSON the user never wrote.
#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    severity: BTreeMap<String, String>,
    #[serde(default)]
    timeouts: RawTimeouts,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Default, Deserialize)]
struct RawTimeouts {
    rule: Option<u64>,
    global: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    id: Option<String>,
    language: Option<String>,
    severity: Option<String>,
    card: Option<RawCard>,
    query: Option<String>,
    #[serde(default)]
    gates: Gates,
    timeout: Option<u64>,
    has_check: bool,
    has_reduce: bool,
}

#[derive(Debug, Deserialize)]
struct RawCard {
    message: Option<String>,
    remediation: Option<String>,
    examples: Option<RawExamples>,
}

#[derive(Debug, Deserialize)]
struct RawExamples {
    bad: Option<String>,
    good: Option<String>,
}

/// The name of the synthetic entry module.
///
/// It has to sit inside the rules root, because the resolver treats a module's name as its
/// path when resolving that module's imports.
const ENTRY: &str = "__lanekeep_entry__.js";

/// The script that reduces the config to JSON.
///
/// `has_check` and `has_reduce` are recorded here rather than inferred later, because
/// `JSON.stringify` drops functions and there is no way to tell afterwards whether a rule
/// had a handler or a typo.
const EXTRACT: &str = r"
    (() => {
        const c = globalThis.__lanekeepConfig;
        if (c === null || typeof c !== 'object') return JSON.stringify(null);
        const rules = Array.isArray(c.rules) ? c.rules : [];
        return JSON.stringify({
            include: c.include ?? [],
            exclude: c.exclude ?? [],
            severity: c.severity ?? {},
            timeouts: c.timeouts ?? {},
            rules: rules.map((r) => ({
                id: r?.id ?? null,
                language: r?.language ?? null,
                severity: r?.severity ?? null,
                card: r?.card ?? null,
                query: r?.query ?? null,
                gates: r?.gates ?? {},
                timeout: r?.timeout ?? null,
                has_check: typeof r?.check === 'function',
                has_reduce: typeof r?.reduce === 'function',
            })),
        });
    })()
";

/// Load and validate a configuration.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, the module fails to evaluate, or
/// the result is not shaped like a config.
pub fn load(sandbox: &Sandbox, root: &RuleRoot, config_path: &Path) -> Result<Config, ConfigError> {
    let display = config_path.display().to_string();

    let specifier =
        relative_specifier(root.path(), config_path).ok_or_else(|| ConfigError::Unreadable {
            path: display.clone(),
            detail: "the config file must sit inside the rules root".to_owned(),
        })?;

    let entry = root.path().join(ENTRY);
    let source =
        format!("import config from '{specifier}';\nglobalThis.__lanekeepConfig = config;\n");
    sandbox
        .eval_module(&entry.display().to_string(), &source)
        .map_err(|e| ConfigError::Evaluation {
            path: display.clone(),
            detail: e.to_string(),
        })?;

    let json: String = sandbox.eval(EXTRACT).map_err(|e| ConfigError::Evaluation {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    let raw: Option<RawConfig> = serde_json::from_str(&json).map_err(|e| ConfigError::Shape {
        path: display.clone(),
        detail: e.to_string(),
    })?;
    let raw = raw.ok_or_else(|| ConfigError::Shape {
        path: display.clone(),
        detail: "the default export is not an object — did you forget `export default`?".to_owned(),
    })?;

    build(sandbox, raw, &display)
}

fn build(sandbox: &Sandbox, raw: RawConfig, display: &str) -> Result<Config, ConfigError> {
    let overrides = parse_severity_overrides(&raw.severity, display)?;

    let mut rules = Vec::with_capacity(raw.rules.len());
    for (index, rule) in raw.rules.into_iter().enumerate() {
        rules.push(build_rule(rule, index + 1, display, &overrides)?);
    }

    let mut limits = Limits::default();
    if let Some(ms) = raw.timeouts.rule {
        limits = limits.with_rule_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = raw.timeouts.global {
        limits = limits.with_global_timeout(Duration::from_millis(ms));
    }

    let ruleset_hash = hash_ruleset(sandbox);
    let config_hash = hash_config(&raw.include, &raw.exclude, &overrides, &limits);

    Ok(Config {
        include: raw.include,
        exclude: raw.exclude,
        rules,
        limits,
        ruleset_hash,
        config_hash,
    })
}

fn parse_severity_overrides(
    raw: &BTreeMap<String, String>,
    display: &str,
) -> Result<BTreeMap<RuleId, Severity>, ConfigError> {
    raw.iter()
        .map(|(id, severity)| {
            let id = id.parse::<RuleId>().map_err(|e| ConfigError::Shape {
                path: display.to_owned(),
                detail: format!("in `severity`: {e}"),
            })?;
            let severity = severity
                .parse::<Severity>()
                .map_err(|e| ConfigError::Shape {
                    path: display.to_owned(),
                    detail: format!("in `severity` for `{id}`: {e}"),
                })?;
            Ok((id, severity))
        })
        .collect()
}

fn build_rule(
    raw: RawRule,
    position: usize,
    display: &str,
    overrides: &BTreeMap<RuleId, Severity>,
) -> Result<RuleSpec, ConfigError> {
    let fail = |detail: String| ConfigError::Rule {
        position,
        path: display.to_owned(),
        detail,
    };

    let id = raw
        .id
        .ok_or_else(|| fail("missing `id`".to_owned()))?
        .parse::<RuleId>()
        .map_err(|e| fail(e.to_string()))?;

    // The check that JSON extraction exists to make possible. A rule whose handler is
    // missing or misspelled would otherwise load cleanly and never report, which is
    // indistinguishable from the code being fine.
    if !raw.has_check {
        return Err(fail(format!(
            "`{id}` has no `check` function — a rule without one can never report anything"
        )));
    }

    let query = raw
        .query
        .ok_or_else(|| fail(format!("`{id}` has no `query`")))?;
    if query.trim().is_empty() {
        return Err(fail(format!("`{id}` has an empty `query`")));
    }

    let card = raw
        .card
        .ok_or_else(|| fail(format!("`{id}` has no `card`")))?;
    let examples = card.examples.unwrap_or(RawExamples {
        bad: None,
        good: None,
    });
    let card = RuleCard {
        message: card.message.unwrap_or_default(),
        remediation: card.remediation.unwrap_or_default(),
        examples: Examples {
            bad: examples.bad.unwrap_or_default(),
            good: examples.good.unwrap_or_default(),
        },
    };
    card.validate()
        .map_err(|problems| fail(format!("`{id}` has an unusable card: {problems:?}")))?;

    let declared = raw
        .severity
        .map(|s| s.parse::<Severity>())
        .transpose()
        .map_err(|e| fail(format!("`{id}`: {e}")))?
        .unwrap_or(Severity::Error);

    Ok(RuleSpec {
        // Config severity wins over what the rule declares, per §9.
        severity: overrides.get(&id).copied().unwrap_or(declared),
        id,
        language: raw.language.unwrap_or_else(|| "typescript".to_owned()),
        card,
        query,
        gates: raw.gates,
        timeout: raw.timeout.map(Duration::from_millis),
        has_reduce: raw.has_reduce,
    })
}

/// Hash every module the loader read.
///
/// # A correction to the architecture
///
/// §8 says `ruleset_hash` must be over *canonicalized* rule definitions, so that
/// reformatting does not invalidate while editing a regex does. That was written when rules
/// were declarative data, where canonicalizing means normalizing a parsed value.
///
/// Rules are now TypeScript, and canonicalizing arbitrary TypeScript would mean shipping a
/// formatter and agreeing on its output forever. So this hashes module source bytes:
/// reformatting a rule *does* invalidate its cached results.
///
/// That is over-invalidation, which costs a recompute. The alternative error —
/// under-invalidating and serving results computed by code that no longer exists — is the
/// one §8 exists to prevent, and it is not symmetric with this one.
fn hash_ruleset(sandbox: &Sandbox) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-ruleset-v1");

    if let Some(loaded) = sandbox.loaded_modules() {
        // The map is ordered, so the hash does not depend on load order — which varies with
        // import structure and is not something the user changed.
        for (path, source) in loaded.borrow().iter() {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(&[0]);
            hasher.update(source.as_bytes());
            hasher.update(&[0]);
        }
    }

    *hasher.finalize().as_bytes()
}

/// Hash the configuration values.
///
/// Canonicalized properly, because these *are* structured data: the severity map is ordered
/// so writing the same entries in a different order hashes the same, and the budgets are
/// hashed as numbers rather than as whatever the user typed.
fn hash_config(
    include: &[String],
    exclude: &[String],
    severity: &BTreeMap<RuleId, Severity>,
    limits: &Limits,
) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-config-v1");

    for (label, globs) in [
        (b"include".as_slice(), include),
        (b"exclude".as_slice(), exclude),
    ] {
        hasher.update(label);
        // Include and exclude are order-insensitive in effect, so hashing them in the
        // order written would invalidate on a reordering that changes nothing.
        let mut sorted: Vec<&String> = globs.iter().collect();
        sorted.sort();
        for glob in sorted {
            hasher.update(glob.as_bytes());
            hasher.update(&[0]);
        }
    }

    hasher.update(b"severity");
    for (id, level) in severity {
        hasher.update(id.to_string().as_bytes());
        hasher.update(&[0]);
        hasher.update(level.as_str().as_bytes());
        hasher.update(&[0]);
    }

    hasher.update(b"limits");
    for value in [
        limits.rule_timeout.as_millis(),
        limits.global_timeout.as_millis(),
        limits.memory_bytes as u128,
    ] {
        hasher.update(&value.to_le_bytes());
    }

    *hasher.finalize().as_bytes()
}

/// A `./`-relative specifier from the root to a file inside it.
fn relative_specifier(root: &Path, file: &Path) -> Option<String> {
    let file = file.canonicalize().ok()?;
    let relative = file.strip_prefix(root).ok()?;
    let joined = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("./{joined}"))
}

/// Build a sandbox able to load configuration from a rules root.
///
/// # Errors
///
/// Returns [`ConfigError::Unreadable`] if the sandbox cannot be constructed.
pub fn sandbox_for(
    root: &RuleRoot,
    typescript: std::sync::Arc<dyn lanekeep_js::Language>,
    javascript: std::sync::Arc<dyn lanekeep_js::Language>,
) -> Result<Sandbox, ConfigError> {
    let limits = Limits::default();
    Sandbox::with_modules(
        limits,
        RunClock::start(limits.global_timeout),
        root.clone(),
        typescript,
        javascript,
    )
    .map_err(|e| ConfigError::Unreadable {
        path: root.path().display().to_string(),
        detail: e.to_string(),
    })
}

/// Where a config file is expected, relative to a project root.
#[must_use]
pub fn default_config_paths(project_root: &Path) -> Vec<PathBuf> {
    [
        "lanekeep.config.ts",
        "lanekeep.config.js",
        "lanekeep.config.mjs",
    ]
    .iter()
    .map(|name| project_root.join(name))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use lanekeep_lang_js::{JavaScript, TypeScript};

    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("lanekeep-config-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("creates dir");
            let fixture = Self { dir };
            fixture.write_all(files);
            fixture
        }

        fn write_all(&self, files: &[(&str, &str)]) {
            for (path, contents) in files {
                let full = self.dir.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).expect("creates parent");
                }
                fs::write(&full, contents).expect("writes");
            }
        }

        fn load_config(&self) -> Result<Config, ConfigError> {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            let sandbox =
                sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
            load(&sandbox, &root, &self.dir.join("lanekeep.config.ts"))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// A minimal, valid rule module.
    fn rule(id: &str) -> String {
        format!(
            "import {{ defineRule }} from 'lanekeep';\n\
             export default defineRule({{\n\
               id: '{id}',\n\
               query: '(identifier) @id',\n\
               card: {{ message: 'no', remediation: 'do this', examples: {{ bad: 'a', good: 'b' }} }},\n\
               check(ctx, m) {{ ctx.report(m.id); }},\n\
             }});\n"
        )
    }

    fn config_with(body: &str) -> String {
        format!(
            "import {{ defineConfig }} from 'lanekeep';\n\
             import rule from './rule';\n\
             export default defineConfig({{ {body} }});\n"
        )
    }

    #[test]
    fn loads_a_valid_config() {
        let fixture = Fixture::new(
            "valid",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.config.ts",
                    &config_with(
                        "include: ['src/**/*.ts'], exclude: ['**/*.test.ts'], rules: [rule]",
                    ),
                ),
            ],
        );

        let config = fixture.load_config().expect("loads");
        assert_eq!(config.include, ["src/**/*.ts"]);
        assert_eq!(config.exclude, ["**/*.test.ts"]);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].id.to_string(), "local/example");
        assert_eq!(config.rules[0].card.message, "no");
        assert!(!config.rules[0].has_reduce);
    }

    #[test]
    fn a_rule_without_a_check_function_is_rejected() {
        // The failure JSON extraction exists to catch. Without this the rule loads, never
        // fires, and looks exactly like the code being clean.
        //
        // The handler is named `onMatch` rather than a misspelling of `check`, because the
        // spell checker flags a real typo in source even inside a fixture — and allowing it
        // globally to keep the joke would be a poor trade. What matters is that `check` is
        // absent, not how it came to be.
        let fixture = Fixture::new(
            "no-check",
            &[
                (
                    "rule.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     export default defineRule({\n\
                       id: 'local/typo',\n\
                       query: '(identifier) @id',\n\
                       card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
                       onMatch(ctx, m) {},\n\
                     });\n",
                ),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );

        let err = fixture.load_config().expect_err("must be rejected");
        let rendered = err.to_string();
        assert!(rendered.contains("check"), "{rendered}");
        assert!(rendered.contains("never report"), "{rendered}");
    }

    #[test]
    fn a_rule_with_a_bare_id_is_rejected() {
        let fixture = Fixture::new(
            "bare-id",
            &[
                ("rule.ts", &rule("example")),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );
        let rendered = fixture
            .load_config()
            .expect_err("must be rejected")
            .to_string();
        assert!(rendered.contains("namespace"), "{rendered}");
    }

    #[test]
    fn a_rule_with_an_unusable_card_is_rejected() {
        let fixture = Fixture::new(
            "bad-card",
            &[
                (
                    "rule.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     export default defineRule({\n\
                       id: 'local/empty',\n\
                       query: '(identifier) @id',\n\
                       card: { message: '', remediation: '', examples: { bad: '', good: '' } },\n\
                       check() {},\n\
                     });\n",
                ),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );
        assert!(fixture.load_config().is_err());
    }

    #[test]
    fn a_missing_default_export_says_so() {
        // The engine catches this at link time, before extraction runs, and its message is
        // better than a generic one would be — it names the module and the missing export.
        let fixture = Fixture::new(
            "no-default",
            &[
                ("rule.ts", &rule("local/x")),
                ("lanekeep.config.ts", "export const notDefault = 1;\n"),
            ],
        );
        let rendered = fixture
            .load_config()
            .expect_err("must be rejected")
            .to_string();
        assert!(rendered.contains("default"), "{rendered}");
    }

    #[test]
    fn a_default_export_that_is_not_an_object_says_so() {
        // This one does reach our own check: the export exists, so the engine is happy,
        // and only the shape is wrong.
        let fixture = Fixture::new(
            "default-not-object",
            &[
                ("rule.ts", &rule("local/x")),
                ("lanekeep.config.ts", "export default 42;\n"),
            ],
        );
        let rendered = fixture
            .load_config()
            .expect_err("must be rejected")
            .to_string();
        assert!(rendered.contains("export default"), "{rendered}");
    }

    #[test]
    fn config_severity_overrides_what_the_rule_declares() {
        let fixture = Fixture::new(
            "severity",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.config.ts",
                    &config_with("rules: [rule], severity: { 'local/example': 'warn' }"),
                ),
            ],
        );
        let config = fixture.load_config().expect("loads");
        assert_eq!(config.rules[0].severity, Severity::Warn);
    }

    #[test]
    fn timeouts_fall_back_to_the_defaults() {
        let fixture = Fixture::new(
            "timeouts-default",
            &[
                ("rule.ts", &rule("local/example")),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );
        let config = fixture.load_config().expect("loads");
        assert_eq!(config.limits, Limits::default());
    }

    #[test]
    fn timeouts_can_be_overridden() {
        let fixture = Fixture::new(
            "timeouts-set",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.config.ts",
                    &config_with("rules: [rule], timeouts: { rule: 2000, global: 30000 }"),
                ),
            ],
        );
        let config = fixture.load_config().expect("loads");
        assert_eq!(config.limits.rule_timeout, Duration::from_secs(2));
        assert_eq!(config.limits.global_timeout, Duration::from_secs(30));
    }

    // --- hashing --------------------------------------------------------------------

    #[test]
    fn the_ruleset_hash_covers_an_imported_helper() {
        // The §8 property, and the reason the loader records what it read rather than the
        // config naming its own inputs. A rule importing a helper has to invalidate when
        // that helper changes — nothing else in the system knows the helper was involved.
        let files: &[(&str, &str)] = &[
            ("helper.ts", "export const QUERY = '(identifier) @id';\n"),
            (
                "rule.ts",
                "import { defineRule } from 'lanekeep';\n\
                 import { QUERY } from './helper';\n\
                 export default defineRule({\n\
                   id: 'local/example',\n\
                   query: QUERY,\n\
                   card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
                   check() {},\n\
                 });\n",
            ),
            ("lanekeep.config.ts", ""),
        ];
        let fixture = Fixture::new("helper-hash", files);
        fixture.write_all(&[("lanekeep.config.ts", &config_with("rules: [rule]"))]);

        let before = fixture.load_config().expect("loads").ruleset_hash;

        fixture.write_all(&[("helper.ts", "export const QUERY = '(string) @s';\n")]);
        let after = fixture.load_config().expect("loads").ruleset_hash;

        assert_ne!(
            hex(&before),
            hex(&after),
            "changing an imported helper must invalidate the ruleset hash"
        );
    }

    #[test]
    fn the_ruleset_hash_is_stable_when_nothing_changed() {
        let fixture = Fixture::new(
            "stable-hash",
            &[
                ("rule.ts", &rule("local/example")),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );
        let first = fixture.load_config().expect("loads").ruleset_hash;
        let second = fixture.load_config().expect("loads").ruleset_hash;
        assert_eq!(hex(&first), hex(&second));
    }

    #[test]
    fn the_config_hash_ignores_glob_order() {
        // Include and exclude are order-insensitive in effect, so reordering them must not
        // throw away a warm cache for a change that alters nothing.
        let make = |globs: &str| {
            Fixture::new(
                &format!("glob-order-{}", globs.len()),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.config.ts",
                        &config_with(&format!("rules: [rule], include: {globs}")),
                    ),
                ],
            )
            .load_config()
            .expect("loads")
            .config_hash
        };

        assert_eq!(
            hex(&make("['a/**', 'b/**']")),
            hex(&make("['b/**', 'a/**' ]")),
            "reordering globs must not change the config hash"
        );
    }

    #[test]
    fn the_config_hash_changes_with_severity() {
        let make = |extra: &str, tag: &str| {
            Fixture::new(
                &format!("severity-hash-{tag}"),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.config.ts",
                        &config_with(&format!("rules: [rule]{extra}")),
                    ),
                ],
            )
            .load_config()
            .expect("loads")
            .config_hash
        };

        assert_ne!(
            hex(&make("", "none")),
            hex(&make(", severity: { 'local/example': 'warn' }", "warn")),
            "changing a severity must invalidate"
        );
    }

    #[test]
    fn the_config_hash_changes_with_a_timeout() {
        let make = |extra: &str, tag: &str| {
            Fixture::new(
                &format!("timeout-hash-{tag}"),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.config.ts",
                        &config_with(&format!("rules: [rule]{extra}")),
                    ),
                ],
            )
            .load_config()
            .expect("loads")
            .config_hash
        };

        assert_ne!(
            hex(&make("", "d")),
            hex(&make(", timeouts: { rule: 5000 }", "t"))
        );
    }

    #[test]
    fn hex_renders_a_full_hash() {
        assert_eq!(hex(&[0u8; 32]).len(), 64);
        assert_eq!(hex(&[0xab; 32]), "ab".repeat(32));
    }
}
