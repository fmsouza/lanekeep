//! Configuration loading and canonicalized hashing for lanekeep.
//!
//! Loads `lanekeep.config.ts` or `lanekeep.json`, resolves the rule graph, and derives the
//! hashes feeding the cache key.
//!
//! # How the config is read
//!
//! A `lanekeep.config.ts` is a TypeScript module, so reading it means running it. A
//! synthetic entry module imports the config's default export into a global, and a second
//! evaluation hands back `JSON.stringify` of the parts that are data.
//!
//! Going through JSON rather than reaching into engine values is deliberate. It keeps
//! every value crossing the boundary plainly serializable, it makes the whole extraction
//! one testable string, and it sidesteps threading engine lifetimes through this crate.
//!
//! The one thing it cannot carry is a function, and `check` is a function. So the
//! extraction separately records whether each rule has a callable `check` and `reduce`.
//! Without that, a rule whose handler was misspelled would load cleanly and silently never
//! fire — the worst failure this tool can have, because it looks exactly like passing.
//!
//! A `lanekeep.json` is not a program, and is read as what it is: `src/json.rs` parses,
//! validates and resolves it in Rust, and only a rule reference naming a *TypeScript* rule
//! reaches the sandbox — because that rule's own declaration is the only place its `id`,
//! `query` and `card` exist. The note above `entry_source` in this file records what
//! that cost.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lanekeep_core::{Examples, Gates, Namespace, RuleCard, RuleId, Severity};
use lanekeep_js::{Limits, RuleRoot, RunClock, Sandbox};
use serde::Deserialize;
use thiserror::Error;

/// A 32-byte content hash.
pub type Hash = [u8; 32];

mod json;

pub use json::{ResolvedRule, RuleReference};

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

/// A rule as the config declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSpec {
    /// Zero-based position in the config's `rules` array.
    ///
    /// This is how the engine reaches the handler: the rule object lives in the loaded
    /// config, and indexing into it is what lets a function cross the boundary without
    /// ever being extracted as a value.
    pub index: usize,
    /// Namespaced identifier.
    pub id: RuleId,
    /// Which languages' grammars the query compiles against, and which files the rule runs on.
    ///
    /// A rule runs on a file only when the file's own language is one of these, and it is
    /// then parsed with *that* grammar. Running every rule against every file with a single
    /// declared grammar is what used to turn a `.tsx` file into a tree of `ERROR` nodes —
    /// silently, since a query simply matches nothing inside one.
    pub languages: Vec<String>,
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
    namespaces: Vec<String>,
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
    language: Option<RawLanguages>,
    severity: Option<String>,
    card: Option<RawCard>,
    query: Option<String>,
    #[serde(default)]
    gates: Gates,
    timeout: Option<u64>,
    has_check: bool,
    has_reduce: bool,
}

/// `language: 'tsx'` and `language: ['typescript', 'tsx']` are both ordinary things to write.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawLanguages {
    One(String),
    Many(Vec<String>),
}

impl RawLanguages {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(language) => vec![language],
            Self::Many(languages) => languages,
        }
    }
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
            namespaces: c.namespaces ?? [],
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

/// The entry module the loader evaluates, and — for a JSON config — everything about it
/// that never needed evaluating.
///
/// # The two formats no longer share a mechanism, and what holds them together now
///
/// They used to. A JSON config was compiled into the same module a TypeScript one is
/// imported by, so extraction, validation, hashing and the cache key never learned which
/// format they came from, and `json.rs`'s header said outright that this is why "the two
/// cannot drift." That mechanism is gone from the JSON path: `lanekeep.json` is parsed,
/// validated and resolved in Rust, and its `include`, `exclude`, `namespaces`, `severity`
/// and `timeouts` never become JavaScript at all.
///
/// **That is what the un-coupling costs.** Two code paths can drift where one could not.
/// Three things substitute for the mechanism, and they are named here rather than left
/// implied, because two of them are conventions and only one is enforced.
///
/// *Enforced.* `json::parse` builds the **shared** `RawConfig` with an exhaustive struct
/// literal, so a field added to it is a compile error on the JSON side rather than a setting
/// that quietly stops being carried. This is the one guard that is stronger than what it
/// replaced — the same omission from the old entry module's `format!` string compiled.
///
/// *Convention.* The two paths still converge at [`build`], the only place a `Config` is
/// constructed, a severity override applied, a card validated or a hash taken, so a
/// divergence has to be introduced upstream of a single function rather than anywhere.
///
/// *Convention.* The cache-key properties §8.1 depends on are asserted against **both** paths
/// in this file's tests, deliberately in matched pairs. Nothing enforces that a new property
/// gets both halves; the pairing is named in the tests so that dropping one is visible.
///
/// # Why `lanekeep-js` is still a dependency of this crate
///
/// Because `lanekeep.config.ts` is still evaluated, and will be until the last rule has
/// migrated to a component — the accepted ADR's condition 8. Nothing here is a step toward
/// deleting the sandbox on this crate's own schedule.
///
/// The JSON path also still reaches the sandbox, for one thing and not for configuration: a
/// reference naming a TypeScript rule is imported so its `defineRule` object can be read.
/// That is rule execution, which is the part condition 8 keeps. Nothing else crosses, which
/// `json::tests::no_configuration_data_reaches_the_entry_module` holds the line on.
///
/// # What unblocks removing QuickJS, and what does not
///
/// Un-coupling this path is one of condition 8's two preconditions. **The other is open and
/// this change does not answer it**: the ADR's §7.6 asks what a programmable
/// `lanekeep.config.ts` means once there is no JavaScript sandbox — arbitrary composition
/// logic, a shared preset imported as a module and spread into another config, per
/// `docs/architecture.md` §9. At least three shapes are plausible and no measurement picks
/// between them: configuration stops being programmable and becomes JSON-only; configuration
/// becomes its own component with a config-shaped WIT world; or a minimal JavaScript
/// evaluator is deliberately retained for configuration alone, decoupled from rule
/// execution. It is a decision about what lanekeep's configuration language should be, and
/// nobody has made it.
///
/// This function reading JSON without a sandbox is *not* that decision, and must not be read
/// as evidence for the first shape. It says a config format that was never programmable does
/// not need an evaluator, which was true before this change too.
fn entry_source(
    root: &RuleRoot,
    config_path: &Path,
    display: &str,
) -> Result<(String, Option<json::Parsed>), ConfigError> {
    if json::is_json(config_path) {
        let parsed = json::parse(config_path, root.path())?;
        let source = json::rules_module(&parsed.rules, display)?;
        return Ok((source, Some(parsed)));
    }

    let specifier =
        relative_specifier(root.path(), config_path).ok_or_else(|| ConfigError::Unreadable {
            path: display.to_owned(),
            detail: "the config file must sit inside the rules root".to_owned(),
        })?;
    Ok((
        format!("import config from '{specifier}';\nglobalThis.__lanekeepConfig = config;\n"),
        None,
    ))
}

/// Evaluate the config module into a sandbox, leaving the rule objects reachable.
///
/// Separate from [`load`] because every worker needs the ruleset present in its own engine
/// — a rule's `check` is a function, and a function cannot be moved between runtimes. Each
/// worker therefore evaluates the same modules rather than receiving extracted values.
///
/// # Errors
///
/// Returns [`ConfigError`] when the config sits outside the rules root or fails to
/// evaluate.
pub fn evaluate_into(
    sandbox: &Sandbox,
    root: &RuleRoot,
    config_path: &Path,
) -> Result<(), ConfigError> {
    let display = config_path.display().to_string();
    let entry = root.path().join(ENTRY);
    let (source, _) = entry_source(root, config_path, &display)?;

    sandbox
        .eval_module(&entry.display().to_string(), &source)
        .map_err(|e| ConfigError::Evaluation {
            path: display,
            detail: e.to_string(),
        })
}

/// Load and validate a configuration.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, the module fails to evaluate, or
/// the result is not shaped like a config.
pub fn load(sandbox: &Sandbox, root: &RuleRoot, config_path: &Path) -> Result<Config, ConfigError> {
    let display = config_path.display().to_string();

    let entry = root.path().join(ENTRY);
    let (source, parsed) = entry_source(root, config_path, &display)?;
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

    let extracted: Option<RawConfig> =
        serde_json::from_str(&json).map_err(|e| ConfigError::Shape {
            path: display.clone(),
            detail: e.to_string(),
        })?;
    let extracted = extracted.ok_or_else(|| ConfigError::Shape {
        path: display.clone(),
        detail: "the default export is not an object — did you forget `export default`?".to_owned(),
    })?;

    // A JSON config supplies its own data; exactly one field comes back from the sandbox,
    // and it is spelled out rather than merged, so a field added to `RawConfig` cannot
    // quietly start being read from the wrong side.
    let (raw, resolved) = match parsed {
        Some(parsed) => (
            RawConfig {
                rules: extracted.rules,
                ..parsed.config
            },
            parsed.rules,
        ),
        None => (extracted, Vec::new()),
    };

    build(sandbox, raw, &display, &resolved)
}

fn build(
    sandbox: &Sandbox,
    raw: RawConfig,
    display: &str,
    resolved: &[ResolvedRule],
) -> Result<Config, ConfigError> {
    let overrides = parse_severity_overrides(&raw.severity, display)?;

    // Namespaces this project claims, beyond the two lanekeep defines. Validated for shape
    // here so a malformed one is reported against `namespaces` rather than against whichever
    // rule happened to use it first.
    let mut declared = BTreeSet::new();
    for namespace in &raw.namespaces {
        RuleId::namespace_from_str(namespace).map_err(|e| ConfigError::Shape {
            path: display.to_owned(),
            detail: format!("`namespaces` contains an invalid entry: {e}"),
        })?;
        if namespace == Namespace::LANEKEEP {
            return Err(ConfigError::Shape {
                path: display.to_owned(),
                detail: "`lanekeep` is reserved for rules shipped with lanekeep — a rule's \
                         origin should be readable from its ID"
                    .to_owned(),
            });
        }
        declared.insert(namespace.clone());
    }

    let mut rules = Vec::with_capacity(raw.rules.len());
    for (index, rule) in raw.rules.into_iter().enumerate() {
        rules.push(build_rule(rule, index + 1, display, &overrides, &declared)?);
    }

    let mut limits = Limits::default();
    if let Some(ms) = raw.timeouts.rule {
        limits = limits.with_rule_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = raw.timeouts.global {
        limits = limits.with_global_timeout(Duration::from_millis(ms));
    }

    let ruleset_hash = hash_ruleset(sandbox, resolved);
    let config_hash = hash_config(&raw.include, &raw.exclude, &overrides, &limits, resolved);

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
    declared: &BTreeSet<String>,
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

    // A namespace nobody declared is a typo, and this is the only layer that can tell.
    // Parsing accepts any well-formed namespace so a team can use its own; declaring it is
    // what keeps `lanekep/foo` from becoming a valid ID that quietly matches nothing.
    if !id.namespace().is_built_in() && !declared.contains(id.namespace().as_str()) {
        let mut known: Vec<String> = Namespace::built_ins()
            .iter()
            .map(|n| format!("`{n}`"))
            .collect();
        known.extend(declared.iter().map(|n| format!("`{n}`")));
        return Err(fail(format!(
            "rule namespace `{}` is not declared — add it to `namespaces` in the config, \
             or use one of {}",
            id.namespace(),
            known.join(", ")
        )));
    }

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
        index: position - 1,
        // Config severity wins over what the rule declares, per §9.
        severity: overrides.get(&id).copied().unwrap_or(declared),
        id,
        // Both TypeScript dialects by default, because a rule written for TypeScript is
        // meant for the TypeScript in the project — and in any React codebase most of that
        // lives in `.tsx`, which the TypeScript grammar cannot parse.
        languages: raw.language.map_or_else(
            || vec!["typescript".to_owned(), "tsx".to_owned()],
            RawLanguages::into_vec,
        ),
        card,
        query,
        gates: raw.gates,
        timeout: raw.timeout.map(Duration::from_millis),
        has_reduce: raw.has_reduce,
    })
}

/// Hash the code every rule in this run is made of: modules the loader read, and components.
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
///
/// # Two kinds of rule code, and why both are folded here rather than one replacing the other
///
/// A component's bytes are the same input as a module's source: the code that decided the
/// answer. The plan for this change described the component fold as replacing the module walk,
/// which would be correct in a world where every rule is a component and is a silent
/// under-invalidation in this one — every rule in this tree is TypeScript, and dropping the
/// walk would take the whole ruleset out of the cache key. So both are folded, and the module
/// walk leaves when the last module does.
///
/// A component is hashed by its **bytes and not its path**, sorted by path so the order is
/// fixed. A resolved component path is absolute, and putting it in would make the key depend
/// on where the checkout sits — a cache invalidated by moving a directory, for nothing. Which
/// component a rule *names*, and with which options, is `hash_config`'s to carry; this hash is
/// about the code. Duplicates collapse for the same reason: listing one component twice is a
/// configuration difference, not a different program.
///
/// **A component that cannot be read folds in a marker rather than nothing.** Skipping it
/// would make "the component is missing" and "the component is there" hash alike, which is the
/// shape §8.2 already rules out for tracked reads: absence is an input, because a run that
/// could not read a rule and one that could must not share a key.
fn hash_ruleset(sandbox: &Sandbox, resolved: &[ResolvedRule]) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-ruleset-v2");

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

    let mut components: Vec<&Path> = resolved
        .iter()
        .filter_map(|rule| match &rule.reference {
            RuleReference::Component(path) => Some(path.as_path()),
            RuleReference::Builtin(_) | RuleReference::Module(_) => None,
        })
        .collect();
    components.sort_unstable();
    components.dedup();

    hasher.update(b"components");
    length_prefixed(&mut hasher, &(components.len() as u64).to_le_bytes());
    for path in components {
        match std::fs::read(path) {
            Ok(bytes) => {
                hasher.update(&[1]);
                length_prefixed(&mut hasher, &bytes);
            }
            Err(_) => {
                hasher.update(&[0]);
            }
        }
    }

    *hasher.finalize().as_bytes()
}

/// Hash a variable-length field with its length in front.
///
/// `u64` rather than `usize`, because `usize::to_le_bytes` is four bytes on a 32-bit host
/// and eight on a 64-bit one, and a hash that depends on the width of the machine that
/// computed it is not deterministic. The saturating conversion is unreachable — it needs a
/// field larger than 16 exabytes — and is written this way because a panic on user input is
/// not something this crate does.
fn length_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

/// Hash the configuration values.
///
/// Canonicalized properly, because these *are* structured data: the severity map is ordered
/// so writing the same entries in a different order hashes the same, and the budgets are
/// hashed as numbers rather than as whatever the user typed.
///
/// `resolved` is a JSON config's rule references and their options, and is empty for a
/// TypeScript one — where the same information lives inside the config module's own source
/// and reaches the key through `ruleset_hash` instead. `docs/architecture.md` §8.1 lists
/// options under this hash, and until the JSON path resolved its references in Rust there
/// was nowhere they could be read from: they were interpolated into the synthetic entry
/// module, which `Sandbox::eval_module` evaluates directly rather than through the loader,
/// so it is not among the modules `hash_ruleset` walks. Editing an option in a
/// `lanekeep.json` therefore invalidated nothing, and a warm run kept answering the previous
/// configuration.
fn hash_config(
    include: &[String],
    exclude: &[String],
    severity: &BTreeMap<RuleId, Severity>,
    limits: &Limits,
    resolved: &[ResolvedRule],
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

    // In the order written, which over-invalidates on a reordering that changes nothing —
    // rules are sorted by ID before they are reported, so their position is not an input to
    // any result. That is the same asymmetry `hash_ruleset` documents: a recompute costs
    // time, and serving a result computed under a different configuration costs correctness.
    hasher.update(b"rules");
    for rule in resolved {
        length_prefixed(&mut hasher, rule.specifier.as_bytes());
        // An explicit discriminant for which form the config wrote, because `"x"` and
        // `{"rule": "x"}` are different configurations — one uses a rule as it comes, the
        // other configures it with `null`, and a factory reading `options?.strict` behaves
        // differently under the two. Omitting the tag would leave them distinguished only by
        // the incidental fact that an absent field and a serialized `null` are different
        // lengths, which is true and is not something to depend on.
        if let Some(options) = &rule.options {
            hasher.update(&[1]);
            length_prefixed(&mut hasher, json::literal(options).as_bytes());
        } else {
            hasher.update(&[0]);
        }
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
        // First, so a project holding both is not silently checked against the other one.
        "lanekeep.json",
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
            self.load_named("lanekeep.config.ts")
        }

        fn load_json(&self) -> Result<Config, ConfigError> {
            self.load_named("lanekeep.json")
        }

        fn load_named(&self, name: &str) -> Result<Config, ConfigError> {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            let sandbox =
                sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
            load(&sandbox, &root, &self.dir.join(name))
        }

        /// A sandbox over this fixture, with nothing loaded into it.
        ///
        /// For the component half of `ruleset_hash`, which cannot be reached through `load`:
        /// a `.wasm` reference is refused before a `Config` is built, because this build runs
        /// no components yet. The hash is still where a component's bytes have to be by the
        /// time one runs, and a cache-key input nothing exercises is how one goes missing.
        fn empty_sandbox(&self) -> Sandbox {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox")
        }

        /// A resolved reference to a component inside this fixture.
        fn component(&self, name: &str) -> ResolvedRule {
            ResolvedRule {
                specifier: format!("./{name}"),
                reference: RuleReference::Component(self.dir.join(name)),
                options: None,
            }
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

    /// A rule factory: what `{ "rule": ..., "options": ... }` and `noRestrictedImports({...})`
    /// both name. The options are captured and ignored; what matters here is that a value
    /// reached the rule.
    fn factory_rule(id: &str) -> String {
        format!(
            "import {{ defineRule }} from 'lanekeep';\n\
             export default (options) => defineRule({{\n\
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

    /// A team can group its rules under its own namespace, which `local/` alone does not
    /// allow — everything project-authored ends up in one bucket regardless of who wrote it.
    #[test]
    fn a_declared_namespace_is_accepted() {
        let fixture = Fixture::new(
            "declared-namespace",
            &[
                ("rule.ts", &rule("pera/no-numeric-sizes")),
                (
                    "lanekeep.config.ts",
                    &config_with("namespaces: ['pera'], rules: [rule]"),
                ),
            ],
        );

        let config = fixture.load_config().expect("loads");
        assert_eq!(config.rules[0].id.to_string(), "pera/no-numeric-sizes");
        assert!(!config.rules[0].id.is_built_in());
    }

    /// And the property that made a closed set worth having in the first place: a namespace
    /// nobody declared is a typo, and it fails at load rather than becoming a valid ID that
    /// silently matches nothing.
    #[test]
    fn an_undeclared_namespace_is_rejected() {
        let fixture = Fixture::new(
            "undeclared-namespace",
            &[
                ("rule.ts", &rule("lanekep/no-default-export")),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );

        let error = fixture
            .load_config()
            .expect_err("an undeclared namespace should be refused")
            .to_string();
        assert!(error.contains("lanekep"), "{error}");
        assert!(
            error.contains("namespaces"),
            "should say how to fix it: {error}"
        );
    }

    /// `lanekeep/` stays reserved, so a rule's origin is readable from its ID alone.
    #[test]
    fn the_lanekeep_namespace_cannot_be_claimed() {
        let fixture = Fixture::new(
            "reserved-namespace",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.config.ts",
                    &config_with("namespaces: ['lanekeep'], rules: [rule]"),
                ),
            ],
        );

        let error = fixture
            .load_config()
            .expect_err("claiming the reserved namespace should be refused")
            .to_string();
        assert!(error.contains("reserved"), "{error}");
    }

    /// A rule with no language of its own targets both TypeScript dialects, because in a
    /// React codebase most TypeScript is `.tsx`.
    #[test]
    fn a_rule_defaults_to_both_typescript_dialects() {
        let fixture = Fixture::new(
            "default-languages",
            &[
                ("rule.ts", &rule("local/example")),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );

        let config = fixture.load_config().expect("loads");
        assert_eq!(config.rules[0].languages, ["typescript", "tsx"]);
    }

    /// One or several, both spelled the way a rule author would write them.
    #[test]
    fn a_rule_may_declare_one_language_or_several() {
        for (declaration, expected) in [
            ("language: 'tsx',", vec!["tsx"]),
            (
                "language: ['typescript', 'tsx'],",
                vec!["typescript", "tsx"],
            ),
        ] {
            let module = format!(
                "import {{ defineRule }} from 'lanekeep';\n\
                 export default defineRule({{\n\
                   id: 'local/example',\n\
                 {declaration}\n\
                   query: '(identifier) @id',\n\
                   card: {{ message: 'no', remediation: 'do this', examples: {{ bad: 'a', good: 'b' }} }},\n\
                   check(ctx, m) {{ ctx.report(m.id); }},\n\
                 }});\n"
            );
            let fixture = Fixture::new(
                "language-forms",
                &[
                    ("rule.ts", &module),
                    ("lanekeep.config.ts", &config_with("rules: [rule]")),
                ],
            );

            let config = fixture.load_config().expect("loads");
            assert_eq!(config.rules[0].languages, expected, "{declaration}");
        }
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
    fn the_ruleset_hash_covers_a_components_bytes() {
        // The component half of the same property `the_ruleset_hash_covers_an_imported_helper`
        // asserts for modules: editing the code a rule is made of must invalidate.
        let fixture = Fixture::new("component-bytes", &[("mine.wasm", "\u{0}asm-one")]);
        let sandbox = fixture.empty_sandbox();
        let resolved = [fixture.component("mine.wasm")];

        let before = hash_ruleset(&sandbox, &resolved);
        fixture.write_all(&[("mine.wasm", "\u{0}asm-two")]);
        let after = hash_ruleset(&sandbox, &resolved);

        assert_ne!(
            hex(&before),
            hex(&after),
            "rebuilding a rule component must invalidate its cached results"
        );
    }

    #[test]
    fn the_ruleset_hash_ignores_where_a_component_sits() {
        // A resolved component path is absolute. Hashing it would mean a cache thrown away by
        // moving a checkout, for a change to nothing a rule can observe — and which component
        // a rule *names* is already `config_hash`'s, through the specifier.
        let fixture = Fixture::new(
            "component-path",
            &[("a.wasm", "\u{0}asm-same"), ("nested/b.wasm", "")],
        );
        fixture.write_all(&[("nested/b.wasm", "\u{0}asm-same")]);
        let sandbox = fixture.empty_sandbox();

        assert_eq!(
            hex(&hash_ruleset(&sandbox, &[fixture.component("a.wasm")])),
            hex(&hash_ruleset(
                &sandbox,
                &[fixture.component("nested/b.wasm")]
            )),
            "the same component bytes are the same ruleset wherever they sit"
        );
    }

    #[test]
    fn a_component_that_cannot_be_read_is_not_one_that_can() {
        // §8.2's "absence is a dependency", one level up. A component that is missing and one
        // that is present must not share a key, or adding the file changes nothing until
        // something unrelated invalidates the entry.
        let fixture = Fixture::new("component-absent", &[("a-present.wasm", "")]);
        let sandbox = fixture.empty_sandbox();

        assert_ne!(
            hex(&hash_ruleset(
                &sandbox,
                &[fixture.component("a-present.wasm")]
            )),
            hex(&hash_ruleset(&sandbox, &[fixture.component("b-gone.wasm")])),
            "an unreadable component must not hash as an empty one"
        );

        // And *which* one is missing has to be distinguishable, which is what the marker buys
        // over simply skipping the entry. Two references, one file present at each: skip the
        // absent one and both runs fold the identical bytes, so a ruleset with one rule broken
        // shares a key with the ruleset that has the other one broken. The count is hashed
        // already, so nothing else would notice.
        fixture.write_all(&[("b-present.wasm", "")]);
        assert_ne!(
            hex(&hash_ruleset(
                &sandbox,
                &[
                    fixture.component("a-present.wasm"),
                    fixture.component("b-gone.wasm"),
                ]
            )),
            hex(&hash_ruleset(
                &sandbox,
                &[
                    fixture.component("a-gone.wasm"),
                    fixture.component("b-present.wasm"),
                ]
            )),
            "which component is missing must be part of the hash, not only how many are"
        );
    }

    #[test]
    fn the_ruleset_hash_ignores_the_order_and_the_repetition_of_a_component() {
        // The "change nothing, assert the key does not move" half. `ruleset_hash` is about the
        // code a run is made of; which rules a config lists, in what order and how often, is
        // `hash_config`'s — where the order is deliberately *not* normalized. Sorting and
        // deduplicating here means a config edit that only reorders costs no recompute.
        let fixture = Fixture::new(
            "component-order",
            &[("one.wasm", "\u{0}asm-one"), ("two.wasm", "\u{0}asm-two")],
        );
        let sandbox = fixture.empty_sandbox();
        let one = fixture.component("one.wasm");
        let two = fixture.component("two.wasm");

        let canonical = hex(&hash_ruleset(&sandbox, &[one.clone(), two.clone()]));
        assert_eq!(
            canonical,
            hex(&hash_ruleset(&sandbox, &[two.clone(), one.clone()])),
            "reordering two components is not a different ruleset"
        );
        assert_eq!(
            canonical,
            hex(&hash_ruleset(&sandbox, &[one.clone(), two, one])),
            "naming one component twice is not a different ruleset"
        );
    }

    #[test]
    fn the_ruleset_hash_still_covers_modules_when_a_component_is_present() {
        // The deviation this change makes from its own plan, asserted rather than described.
        // The plan said the component fold *replaces* the module walk; every rule in this tree
        // is TypeScript, so that would have taken the whole ruleset out of the cache key.
        let files: &[(&str, &str)] = &[
            ("rule.ts", &rule("local/example")),
            ("lanekeep.config.ts", ""),
        ];
        let fixture = Fixture::new("component-and-module", files);
        fixture.write_all(&[("lanekeep.config.ts", &config_with("rules: [rule]"))]);
        fixture.write_all(&[("mine.wasm", "\u{0}asm")]);

        let resolved = [fixture.component("mine.wasm")];
        let root = RuleRoot::new(&fixture.dir).expect("canonicalizes");
        let hash_after_loading = |source: &str| {
            fixture.write_all(&[("rule.ts", source)]);
            let sandbox =
                sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
            evaluate_into(&sandbox, &root, &fixture.dir.join("lanekeep.config.ts"))
                .expect("evaluates");
            hash_ruleset(&sandbox, &resolved)
        };

        assert_ne!(
            hex(&hash_after_loading(&rule("local/example"))),
            hex(&hash_after_loading(&rule("local/renamed"))),
            "a module edit must still invalidate when a component is in the ruleset too"
        );
    }

    #[test]
    fn the_config_hash_ignores_glob_order() {
        // Include and exclude are order-insensitive in effect, so reordering them must not
        // throw away a warm cache for a change that alters nothing.
        let make = |globs: &str, tag: &str| {
            Fixture::new(
                &format!("glob-order-{tag}"),
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
            hex(&make("['a/**', 'b/**']", "sorted")),
            hex(&make("['b/**', 'a/**' ]", "reversed")),
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

    /// A JSON rule's options are a cache-key input, and were reaching neither hash.
    ///
    /// The same config in the same directory, one option value edited: before this was
    /// fixed both hashes came back byte-identical, so a warm run kept answering the
    /// previous configuration. `docs/architecture.md` §8.1 lists options under
    /// `config_hash`, and the JSON path is where they are known as data.
    ///
    /// The fixture is rewritten in place rather than built twice under different names.
    /// Two directories would move `ruleset_hash` on their own — it hashes each module's
    /// path alongside its source — which is a difference that looks like the assertion
    /// passing and is not.
    #[test]
    fn the_config_hash_changes_with_a_json_rule_option() {
        let config =
            |options: &str| format!(r#"{{"rules": [{{"rule": "./rule", "options": {options}}}]}}"#);
        let fixture = Fixture::new(
            "json-option-hash",
            &[
                ("rule.ts", &factory_rule("local/example")),
                ("lanekeep.json", &config(r#"{"limit": 1}"#)),
            ],
        );

        let before = fixture.load_json().expect("loads");
        fixture.write_all(&[("lanekeep.json", &config(r#"{"limit": 2}"#))]);
        let after = fixture.load_json().expect("loads");

        assert_ne!(
            hex(&before.config_hash),
            hex(&after.config_hash),
            "editing a rule option must invalidate"
        );
        assert_eq!(
            hex(&before.ruleset_hash),
            hex(&after.ruleset_hash),
            "no module changed, so the ruleset hash must not move — which is exactly why \
             the config hash has to"
        );
    }

    // --- hashing, the JSON path -------------------------------------------------------
    //
    // Matched pairs of the six above. The two formats used to be one mechanism — a JSON
    // config was compiled into the module a TypeScript one is imported by — so asserting
    // these properties once covered both. It no longer does, and these are what replaced
    // that guarantee. A property that holds on one path and not the other is drift, and
    // drift in a cache key is silent: the run completes and answers with yesterday's
    // configuration.

    #[test]
    fn the_ruleset_hash_covers_an_imported_helper_for_json() {
        let fixture = Fixture::new(
            "json-helper-hash",
            &[
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
                ("lanekeep.json", r#"{"rules": ["./rule"]}"#),
            ],
        );

        let before = fixture.load_json().expect("loads").ruleset_hash;
        fixture.write_all(&[("helper.ts", "export const QUERY = '(string) @s';\n")]);
        let after = fixture.load_json().expect("loads").ruleset_hash;

        assert_ne!(
            hex(&before),
            hex(&after),
            "changing an imported helper must invalidate the ruleset hash"
        );
    }

    #[test]
    fn the_ruleset_hash_is_stable_when_nothing_changed_for_json() {
        let fixture = Fixture::new(
            "json-stable-hash",
            &[
                ("rule.ts", &rule("local/example")),
                ("lanekeep.json", r#"{"rules": ["./rule"]}"#),
            ],
        );
        let first = fixture.load_json().expect("loads").ruleset_hash;
        let second = fixture.load_json().expect("loads").ruleset_hash;
        assert_eq!(hex(&first), hex(&second));
    }

    #[test]
    fn the_config_hash_ignores_glob_order_for_json() {
        let make = |globs: &str, tag: &str| {
            Fixture::new(
                &format!("json-glob-order-{tag}"),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.json",
                        &format!(r#"{{"rules": ["./rule"], "include": {globs}}}"#),
                    ),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_eq!(
            hex(&make(r#"["a/**", "b/**"]"#, "sorted")),
            hex(&make(r#"["b/**", "a/**"]"#, "reversed")),
            "reordering globs must not change the config hash"
        );
    }

    /// The same property one level down, for the values only this path can see.
    ///
    /// `serde_json::Map` is a `BTreeMap` in this build, so the options blob serializes in
    /// key order whatever order it was written in. That is a property of a dependency's
    /// feature set rather than of anything written here — `preserve_order` would reverse it
    /// silently, and the only symptom would be a cache that stops hitting.
    #[test]
    fn the_config_hash_ignores_option_key_order() {
        let make = |options: &str, tag: &str| {
            Fixture::new(
                &format!("json-option-order-{tag}"),
                &[
                    ("rule.ts", &factory_rule("local/example")),
                    (
                        "lanekeep.json",
                        &format!(r#"{{"rules": [{{"rule": "./rule", "options": {options}}}]}}"#),
                    ),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_eq!(
            hex(&make(r#"{"a": 1, "b": 2}"#, "sorted")),
            hex(&make(r#"{"b": 2, "a": 1}"#, "reversed")),
            "reordering option keys must not change the config hash"
        );
    }

    #[test]
    fn the_config_hash_changes_with_severity_for_json() {
        let make = |severity: &str, tag: &str| {
            Fixture::new(
                &format!("json-severity-hash-{tag}"),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.json",
                        &format!(r#"{{"rules": ["./rule"], "severity": {severity}}}"#),
                    ),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_ne!(
            hex(&make("{}", "none")),
            hex(&make(r#"{"local/example": "warn"}"#, "warn")),
            "changing a severity must invalidate"
        );
    }

    #[test]
    fn the_config_hash_changes_with_a_timeout_for_json() {
        let make = |timeouts: &str, tag: &str| {
            Fixture::new(
                &format!("json-timeout-hash-{tag}"),
                &[
                    ("rule.ts", &rule("local/example")),
                    (
                        "lanekeep.json",
                        &format!(r#"{{"rules": ["./rule"], "timeouts": {timeouts}}}"#),
                    ),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_ne!(hex(&make("{}", "d")), hex(&make(r#"{"rule": 5000}"#, "t")));
    }

    /// `"x"` and `{ "rule": "x" }` are different configurations and must not hash alike.
    ///
    /// One uses a rule as it comes; the other configures it, with `null`. A rule factory
    /// reading `options?.strict` behaves differently under the two, so a key that could not
    /// tell them apart would serve one's results for the other. The fixture's default export
    /// is deliberately usable both ways, so the *only* difference between the two runs is
    /// the form the config wrote.
    #[test]
    fn the_config_hash_tells_a_bare_rule_from_a_configured_one() {
        let module = "import { defineRule } from 'lanekeep';\n\
             const built = defineRule({\n\
               id: 'local/example',\n\
               query: '(identifier) @id',\n\
               card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },\n\
               check(ctx, m) { ctx.report(m.id); },\n\
             });\n\
             export default Object.assign((options) => built, built);\n";

        let make = |rules: &str, tag: &str| {
            Fixture::new(
                &format!("json-rule-form-{tag}"),
                &[
                    ("rule.ts", module),
                    ("lanekeep.json", &format!(r#"{{"rules": [{rules}]}}"#)),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_ne!(
            hex(&make(r#""./rule""#, "bare")),
            hex(&make(r#"{"rule": "./rule"}"#, "configured")),
            "a rule used as it comes and a rule configured with `null` are not the same run"
        );
    }

    /// `config_hash` says *which* rule was configured, not merely that something was.
    ///
    /// Today `ruleset_hash` would notice this on its own, because two references load two
    /// different modules. It is pinned here anyway, because Task 15 turns that hash into a
    /// path-sorted fold over component bytes, and a property held only by the hash that is
    /// about to be rewritten is a property about to be lost quietly. The two configs below
    /// differ in nothing `config_hash` sees except the specifier.
    #[test]
    fn the_config_hash_tells_apart_two_rules_with_the_same_options() {
        let make = |name: &str| {
            Fixture::new(
                &format!("json-which-rule-{name}"),
                &[
                    (
                        &format!("{name}.ts"),
                        &factory_rule(&format!("local/{name}")),
                    ),
                    (
                        "lanekeep.json",
                        &format!(r#"{{"rules": [{{"rule": "./{name}", "options": {{"x": 1}}}}]}}"#),
                    ),
                ],
            )
            .load_json()
            .expect("loads")
            .config_hash
        };

        assert_ne!(
            hex(&make("a")),
            hex(&make("b")),
            "the same options on a different rule is a different configuration"
        );
    }

    /// The two formats saying the same thing produce the same configuration.
    ///
    /// This is the assertion the shared entry module used to make unnecessary. It cannot
    /// compare the hashes — a TypeScript config is itself a module in the rule graph, so
    /// `ruleset_hash` legitimately differs — but everything a run actually does is decided
    /// by the fields below, and those must agree exactly.
    #[test]
    fn the_two_formats_load_the_same_configuration() {
        let typescript = Fixture::new(
            "parity-ts",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.config.ts",
                    &config_with(
                        "rules: [rule], include: ['src/**/*.ts'], exclude: ['**/*.test.ts'], \
                         severity: { 'local/example': 'warn' }, \
                         timeouts: { rule: 2000, global: 30000 }",
                    ),
                ),
            ],
        )
        .load_config()
        .expect("the TypeScript config loads");

        let json = Fixture::new(
            "parity-json",
            &[
                ("rule.ts", &rule("local/example")),
                (
                    "lanekeep.json",
                    r#"{"rules": ["./rule"], "include": ["src/**/*.ts"],
                        "exclude": ["**/*.test.ts"], "severity": {"local/example": "warn"},
                        "timeouts": {"rule": 2000, "global": 30000}}"#,
                ),
            ],
        )
        .load_json()
        .expect("the JSON config loads");

        assert_eq!(typescript.include, json.include);
        assert_eq!(typescript.exclude, json.exclude);
        assert_eq!(typescript.limits, json.limits);
        assert_eq!(typescript.rules, json.rules);
    }

    /// The un-coupling, as a property of the source rather than of a call graph.
    ///
    /// `src/json.rs` names neither the sandbox crate nor any type this crate's root imports
    /// from it. The crate as a whole still depends on it, and deliberately — see the note
    /// above `entry_source`.
    ///
    /// **Grepping for `lanekeep_js` alone is not enough, and the gap is the spelling a
    /// refactor would reach for first.** The `use lanekeep_js::{…}` below is at the crate
    /// root, and a `use` at the root is in scope for every descendant module, so this
    /// compiles inside `json.rs`, reaches the sandbox, and contains no `lanekeep_js` at all:
    ///
    /// ```ignore
    /// use crate::{ConfigError, Sandbox};
    /// fn probe(s: &Sandbox) -> bool { s.eval::<bool>("true").unwrap_or(false) }
    /// ```
    ///
    /// The forbidden names are therefore read out of that import line rather than listed
    /// here, so importing a fifth type from that crate extends this check instead of quietly
    /// outgrowing it.
    ///
    /// What it does not cover, stated rather than left to be discovered: reaching the sandbox
    /// without naming a type, through some crate-level function that takes one. No such
    /// function exists for `json.rs` to call today. This is a source check, not a proof.
    #[test]
    fn the_json_path_names_nothing_from_the_sandbox_crate() {
        let root = include_str!("lib.rs");
        let import = root
            .lines()
            .find(|line| line.starts_with("use lanekeep_js::{"))
            .expect("the crate root imports the sandbox crate in one braced list");

        let mut forbidden: Vec<&str> = import
            .trim_start_matches("use lanekeep_js::{")
            .trim_end_matches("};")
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        assert!(
            forbidden.len() > 1
                && forbidden
                    .iter()
                    .all(|n| n.chars().all(char::is_alphanumeric)),
            "the import list should have parsed into type names: {forbidden:?}"
        );
        forbidden.push("lanekeep_js");

        let source = include_str!("json.rs");
        for name in forbidden {
            assert!(
                !source.contains(name),
                "src/json.rs must resolve a JSON config without the sandbox, and it names \
                 `{name}`"
            );
        }
    }

    #[test]
    fn hex_renders_a_full_hash() {
        assert_eq!(hex(&[0u8; 32]).len(), 64);
        assert_eq!(hex(&[0xab; 32]), "ab".repeat(32));
    }
}
