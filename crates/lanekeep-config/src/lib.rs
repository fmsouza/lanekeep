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
use lanekeep_js::{Limits, ResolveError, RuleRoot, RunClock, Sandbox};
use lanekeep_wasm::{RuleSet, WasmEngine, WasmRuntime};
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
    ///
    /// **It is the position in the config and the position in the entry module's array, and
    /// those have to stay one number.** A component-backed rule has no entry in that array —
    /// its handlers are not JavaScript — so `json::rules_module` emits a `null` placeholder to
    /// hold its place rather than closing the gap. Numbering the array separately would leave
    /// every rule after a component pointing at its neighbor's handler: the call succeeds, and
    /// the violations are attributed to the wrong rule.
    ///
    /// **It is therefore not a position in [`Config::rules`], and is not unique across it.** A
    /// component hosts a list of rules, so one entry in the config's array — one placeholder —
    /// can produce several `RuleSpec`s, and every one of them carries the position of the
    /// *reference*. Which of the component's own rules a spec is lives on
    /// [`ComponentRule::index`], and the two numberings answer different questions: this one
    /// names a slot in the entry module, that one names a rule inside a compiled program.
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
    /// The compiled component this rule's handlers live in, or `None` for a TypeScript rule.
    ///
    /// **This is what sends a rule to one engine or the other.** `lanekeep-engine` runs a rule
    /// with `None` through `lanekeep-js` and a rule with `Some` through `lanekeep-wasm`, in the
    /// same run over the same corpus — the decision is a property of the rule and is made here,
    /// where a rule is described, rather than by the engine guessing from anything else.
    ///
    /// Every other field of a component-backed rule is the component's own answer to
    /// `metadata`, read once here at config load. There is no config syntax carrying an `id`, a
    /// `query` or a card beside a `.wasm` reference, and there deliberately never was: a second
    /// description of a rule is drift that has to be kept in step with the first.
    pub component: Option<ComponentRule>,
}

/// Where a component-backed rule's code is, and what it is configured with.
///
/// **One value rather than two fields, because the two cannot be independently true.** A rule
/// backed by a component is always configured — with `null` when the config named it with no
/// options, which is the shape `crates/lanekeep-wasm/wit/world.wit` declares so that a guest
/// has one code path rather than two — and a rule that is not backed by a component has no
/// `configure` to reach. Splitting them would make "a component with nothing to configure it
/// with" and "options belonging to no component" representable, and both are states nothing
/// downstream knows what to do with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentRule {
    /// Where the bytes came from: a path confined to the rules root, or `lanekeep/<name>` for
    /// a built-in embedded in the binary.
    ///
    /// Kept for diagnostics and for the order [`ComponentBytes`] are folded into
    /// `ruleset_hash` in. Nothing reads the file again — see [`ComponentRule::bytes`].
    ///
    /// **A built-in's is a specifier rather than a path, and cannot collide with one.** A
    /// confined path is absolute, because `RuleRoot::confine` canonicalizes; `lanekeep/no-unwrap`
    /// is relative. So a project that happens to have `lanekeep/no-unwrap.wasm` inside its rules
    /// root sorts and dedups separately, as two different rules should.
    pub path: PathBuf,
    /// Which of the component's rules this is: an index into what its `rules` export lists.
    ///
    /// **A component hosts a list, so naming one is a position and not merely a file.** Every
    /// export but `rules` takes this index, so it is what tells `configure`, `metadata`,
    /// `check` and `reduce` which rule they are being asked about. A component hosting one
    /// rule is `0`, which is what every reference resolved to before a component could host
    /// more than one.
    ///
    /// It is folded into `ruleset_hash` beside the component's identity rather than being
    /// carried only for execution: two rules of one component share every byte of code, so the
    /// index is the whole of what distinguishes the programs they run. Without it, "rule 0 and
    /// rule 1 of this component" and "rule 0 of this component, twice" are one cache key.
    pub index: u32,
    /// What `configure` is called with, as JSON — `"null"` for a rule named with no options.
    ///
    /// A string rather than a `serde_json::Value` because that is what crosses the boundary:
    /// a component cannot close over a host-supplied value the way a JavaScript factory does,
    /// so its options arrive as data. Serializing once here also fixes the bytes, which
    /// matters because they are what every worker's `configure` is handed.
    pub options: String,
    /// The component itself, read exactly once.
    ///
    /// **The rule that was described has to be the rule that runs.** The bytes used to be read
    /// three times in a run — once to ask the component what it is, once to hash it, once to
    /// execute it — and a file that changed between those reads would give metadata from one,
    /// a cache key from a second and handlers from a third, with nothing to notice. That is
    /// the same property the TypeScript path already has for free: `hash_ruleset` folds what
    /// `RuleLoader` actually consumed, not a second read of the same paths.
    ///
    /// So they are read once, here, and carried: `metadata` is read from them, `ruleset_hash`
    /// folds them, and `lanekeep-engine` loads the component from them rather than from the
    /// path beside them.
    ///
    /// Behind an [`std::sync::Arc`], because a `RuleSpec` is cloned per rule when the engine
    /// prepares and a per-rule copy of a megabyte is a cost with nothing to buy it.
    pub bytes: ComponentBytes,
    /// Whether these exact bytes were folded into the `ruleset_hash` of the `Config` this
    /// rule sits in.
    ///
    /// `true` for every `ComponentRule` `describe_components` builds — the only constructor
    /// in this crate, and its output is exactly what `build` folds into `ruleset_hash` a few
    /// lines later, over the very `rules` this value ends up attached to. Private, so nothing
    /// outside this crate can construct one that claims coverage it does not have: the only
    /// other way to get a `ComponentRule` is [`ComponentRule::uncounted`], which is honest
    /// about the alternative.
    ///
    /// This is `Engine::caching`'s one input for the question its field doc calls "asking
    /// where the field came from" — a `RuleSpec` an embedder or a test attaches after
    /// `lanekeep_config::load` returns carries a component whose bytes reached no hash, and
    /// `lanekeep-engine` reads this flag to refuse the cache for exactly that run.
    counted_in_ruleset_hash: bool,
}

impl ComponentRule {
    /// Whether these bytes are folded into the `ruleset_hash` of the `Config` they arrived
    /// with — see the field.
    #[must_use]
    pub const fn counted_in_ruleset_hash(&self) -> bool {
        self.counted_in_ruleset_hash
    }

    /// Build a `ComponentRule` outside `lanekeep_config::load`.
    ///
    /// **Whatever this produces is not folded into any `Config`'s `ruleset_hash`,** because
    /// nothing here computes one — that happens exactly once, inside `load`, over whichever
    /// rules were in `Config.rules` at the moment it returned. This is for an embedder, or a
    /// test, that attaches a component to a `RuleSpec` afterward: `lanekeep-engine`'s own
    /// component tests are exactly that, which is why `Engine::caching` refuses the cache for
    /// a run carrying one of these.
    #[must_use]
    pub fn uncounted(
        path: PathBuf,
        index: u32,
        options: String,
        bytes: impl Into<ComponentBytes>,
    ) -> Self {
        Self {
            path,
            index,
            options,
            bytes: bytes.into(),
            counted_in_ruleset_hash: false,
        }
    }
}

/// A component's bytes, shared rather than copied.
///
/// A newtype for one reason: [`RuleSpec`] derives `Debug`, and a bare byte slice renders every
/// byte of a forty-kilobyte artifact into any assertion message that prints a rule. This
/// prints what a reader can act on — how many bytes there are — and the equality that
/// `Config`'s own `PartialEq` needs is still over the content.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentBytes(std::sync::Arc<[u8]>);

impl ComponentBytes {
    /// The bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for ComponentBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl std::fmt::Debug for ComponentBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentBytes")
            .field("len", &self.0.len())
            .finish()
    }
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
        let parsed = json::parse(config_path, root.path(), root.builtin_components())?;
        let source = json::rules_module(&parsed.rules);
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
    load_with(sandbox, root, config_path, LoadOptions::default())
}

/// What a load needs beyond the config file, for a caller that has more to say than [`load`]
/// can carry.
///
/// A struct rather than two more parameters, for the reason `lanekeep-cli`'s `CheckOptions`
/// gives: `artifacts` and the config path are both `&Path`, and adjacent parameters of one
/// type are the shape that gets silently transposed at a call site.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadOptions<'a> {
    /// A project root under which compiled components may be cached, or `None` for a load with
    /// nowhere to write.
    ///
    /// **This is the difference between a component costing tens of milliseconds per config load
    /// and costing nothing.** With `None`, `describe_components` compiles every component from
    /// scratch to ask it what it is, throws the compilation away, and the engine compiles the
    /// same bytes again at prepare time. Measured on the release binary, one component added
    /// ~58 ms to a `lanekeep rules` that checks no files at all, and two added ~116 ms — against
    /// a §15 warm-run budget of 25 ms for the whole invocation. Config load runs per LSP request,
    /// per MCP tool call and per `--watch` iteration, so this is paid on every one of them.
    ///
    /// Given a root, both loads write and map artifacts under the same [`COMPONENT_CACHE_PATH`],
    /// keyed on the specifier and the bytes — so the first run compiles once instead of twice and
    /// every later run maps what that run wrote. The two loaders agree because both build their
    /// `wasmtime::Engine` with `WasmEngine::new`; an artifact a different build wrote fails to
    /// deserialize and is discarded rather than trusted.
    ///
    /// Named by the caller rather than inferred, because a rules root is the project root only by
    /// the CLI's choice — `lanekeep-testkit` anchors one at a temporary fixture directory — and
    /// guessing would make loading a config write somewhere nobody asked for.
    ///
    /// [`COMPONENT_CACHE_PATH`]: lanekeep_wasm::COMPONENT_CACHE_PATH
    pub artifacts: Option<&'a Path>,

    /// Overrides `timeouts.global` from the config file, for a caller holding a more specific
    /// statement — `--timeout`, which a user typed on this run.
    ///
    /// **It has to arrive here rather than be applied to the returned [`Config`], because config
    /// load is itself a phase that runs guest code.** `describe_components` instantiates,
    /// `configure`s and calls `metadata` on every component under a clock of its own, and that
    /// clock is built before this function returns. A caller that loaded first and assigned to
    /// `Config::limits` afterwards would leave that phase governed by the config file's number
    /// while the message a breach prints tells the user to raise it with `--timeout` — advice
    /// that could not work. `AGENTS.md` records the original instance of exactly this shape.
    pub global_timeout: Option<Duration>,
}

/// [`load`], with everything a caller knows that the config file does not.
///
/// See [`LoadOptions`] for what each field buys and why it has to be known before the load
/// rather than applied to the [`Config`] it returns.
///
/// # Errors
///
/// As [`load`].
pub fn load_with(
    sandbox: &Sandbox,
    root: &RuleRoot,
    config_path: &Path,
    options: LoadOptions<'_>,
) -> Result<Config, ConfigError> {
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

    build(sandbox, root, raw, &display, &resolved, options)
}

fn build(
    sandbox: &Sandbox,
    root: &RuleRoot,
    raw: RawConfig,
    display: &str,
    resolved: &[ResolvedRule],
    options: LoadOptions<'_>,
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

    // The budgets, worked out before anything runs under them. `describe_components` executes
    // guest code — instantiation, `configure`, `metadata` — and a component asked what it is
    // under a budget the config did not set is a limit that was parsed and then dropped, which
    // `AGENTS.md` records as the shape of the `--timeout` bug: accepted, validated, ignored.
    //
    // **The caller's override is folded in *here*, not applied to the `Config` this returns.**
    // That is the same bug in a new phase, and it was live for the length of this branch: the CLI
    // loaded the config, then assigned `--timeout` to `loaded.limits`, one statement after the
    // phase it was meant to govern had already finished. A component whose `configure` overran
    // failed with a message ending "raise it with `--timeout`", and raising it changed nothing.
    // Resolving it before `describe_components` is what makes one number govern both phases.
    let mut limits = Limits::default();
    if let Some(ms) = raw.timeouts.rule {
        limits = limits.with_rule_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = raw.timeouts.global {
        limits = limits.with_global_timeout(Duration::from_millis(ms));
    }
    if let Some(global) = options.global_timeout {
        limits = limits.with_global_timeout(global);
    }

    // Every component in the config, asked what it is. Once, here, before a `RuleSpec` exists
    // — not per worker: instantiation is 82 to 96 times the cost of not instantiating, which
    // is why `lanekeep_wasm::WasmRuntime::rule` defers it, and reading metadata through a
    // worker's runtime would undo that for every rule in the set.
    let mut described = describe_components(root, resolved, display, limits, options.artifacts)?;

    let mut rules = Vec::with_capacity(raw.rules.len());
    for (index, rule) in raw.rules.into_iter().enumerate() {
        // A component's entry in `raw.rules` is the placeholder `rules_module` emitted for it,
        // carrying nothing; what describes it is its own `metadata`. The two lists are indexed
        // alike by construction, which is the whole reason the placeholder is there.
        //
        // **One reference, one placeholder, and any number of rules.** A component hosts a list,
        // so a single entry in the config's array can produce several `RuleSpec`s — every one of
        // them carrying `index + 1` as its position, because that is where the *reference* sits
        // and the entry module has exactly one slot for it. A TypeScript rule after a component
        // therefore keeps its own position whatever the component turned out to hold, which is
        // what `RuleSpec::index` has to be true of.
        match described.get_mut(index).and_then(Option::take) {
            Some(hosted) => {
                for rule in hosted {
                    rules.push(build_rule(
                        rule.raw,
                        index + 1,
                        display,
                        &overrides,
                        &declared,
                        Some(rule.component),
                    )?);
                }
            }
            None => rules.push(build_rule(
                rule,
                index + 1,
                display,
                &overrides,
                &declared,
                None,
            )?),
        }
    }

    // Every description has to have been claimed by a rule. One left over means the entry
    // module's array and the config's rule list came out different lengths, and the loop above
    // would then have dropped a component rule without saying so — a configured rule that
    // silently checks nothing is the failure this tool exists not to produce. Unreachable while
    // `rules_module` emits one array entry per reference, which is exactly why it is asserted
    // rather than assumed: the placeholder is what makes it true, and a future edit that
    // removed it would find this instead of a wrong answer.
    if let Some(position) = described.iter().position(Option::is_some) {
        return Err(ConfigError::Rule {
            position: position + 1,
            path: display.to_owned(),
            detail: "this component reached no rule — the entry module's rule array and the \
                     config's rule list are not the same length"
                .to_owned(),
        });
    }

    // The components, in the order the config listed them, taken back off the rules that were
    // just built — so what is hashed is what was described and what will run, rather than a
    // fresh look at the same paths.
    let components: Vec<&ComponentRule> = rules
        .iter()
        .filter_map(|rule| rule.component.as_ref())
        .collect();

    let ruleset_hash = hash_ruleset(sandbox, &components);
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

/// Ask every component the config names which rules it hosts, and what each of them is.
///
/// One entry per resolved reference, `Some` for a component and `None` for anything else, so
/// the answer is indexed by the config's own rule position — the same numbering
/// `json::rules_module`'s placeholder preserves.
///
/// # One reference, a list of rules
///
/// **A component hosts a list and a reference names the component, so the entry is a `Vec`.**
/// Every export but `rules` takes an index into that list, so describing a component means
/// enumerating it first and then asking about each rule by position. A component hosting one
/// rule — every component this repository shipped before this — produces a one-element list and
/// reads exactly as it did.
///
/// A reference's options reach *every* rule the component hosts, because a reference names the
/// component and there is no syntax naming one rule inside it. That is the right shape for the
/// case that exists — a component built to host a family of related rules, configured as a
/// family — and it is not the shape a built-in wants, where `lanekeep/no-default-export` has to
/// mean one rule of a shared artifact. That is a *resolution* question rather than a
/// description one: it is answered by what `json::classify` hands back, not here.
///
/// # Once for the run, and deliberately not through a worker's runtime
///
/// Every component is compiled, instantiated, configured and asked about each of its rules
/// here. That is the cost `lanekeep_wasm::WasmRuntime::rule` exists to avoid paying per worker
/// — #96's spike measured eager instantiation at 82 to 96 times the lazy arrangement — and it
/// is paid exactly once, before any worker exists, because a rule that cannot describe itself
/// cannot be run at all. Nothing built here outlives this function: the engine, the rule set
/// and the runtime are dropped on the way out, and what survives is the metadata and the path.
///
/// **The enumeration costs one instantiation per component that the description then repeats,**
/// because `RuleSet::add` takes an index and cannot discover one — `rules` is an export, so
/// asking needs a store and an instance, and a rule set holds neither. The throwaway instance
/// lives in a runtime of its own, built and dropped inside the loop, so that at most one
/// instance beyond the description's own is resident at a time rather than one per component.
///
/// # What each answer is for
///
/// `metadata` fills every field of the `RuleSpec` a TypeScript rule fills from its own
/// `defineRule` call, and it goes through `build_rule` exactly as an extracted TypeScript rule
/// does — so a component's id, namespace, card, query and severity are validated by the same
/// code, and a component cannot smuggle past a check a TypeScript rule has to satisfy.
///
/// `has-check` and `has-reduce` are asked rather than assumed, which closes the one place a
/// component used to be taken at its config's word about a question it can answer itself.
///
/// `configure` is not called here and is not skipped: `RuleSet::add` records the options and
/// `WasmRuntime::rule` hands them over on the way to the instance `metadata` is read from. So a
/// component that refuses its options fails at config load, naming the rule and carrying the
/// guest's own message, and the same call happens again on every worker that later builds an
/// instance of its own.
///
/// `rules` is the one export asked *before* configuration, and it is why the world splits it
/// from `metadata` rather than returning a list of those. A factory rule's card and query come
/// from applying the factory to its options, so metadata has to be read after `configure`; but
/// configuring rule *i* means knowing that *i* exists. A rule's id cannot depend on its
/// options — the id is how a config names the rule in the first place — so the ids enumerate
/// first and everything else follows configuration.
///
/// # Confinement, before a byte is read — and a built-in has nothing to confine
///
/// A built-in component is embedded in this binary. There is no path in the config, no file on
/// disk and nothing to canonicalize, so the paragraphs below are about a `.wasm` *path*
/// reference and only about that. That is not a weaker check for built-ins; it is the absence
/// of the thing the check exists to constrain, and it is the same reason a built-in module
/// cannot be shadowed by a project file.
///
/// A rule reference is a string in a config file and a component is *executed*, so where it is
/// allowed to point is a trust boundary rather than a convenience. `json::classify` joins the
/// specifier against the rules root and normalizes it, which is purely lexical and does not
/// confine anything: `Path::join` lets an absolute specifier replace the root outright, and no
/// lexical rule can see through a symlink.
///
/// [`RuleRoot::confine`] is the check, and it is the containment half of the one a module
/// import goes through rather than a second set written here: the lexical test that refuses
/// `../../evil.wasm` whatever is on disk, then the canonicalization that refuses a symlink
/// pointing out of the root. It runs before [`std::fs::read`], so a reference that escapes is
/// refused without its bytes ever being loaded, let alone compiled or instantiated.
///
/// **Containment is all of it, and a module import is held to more.** `RuleRoot::resolve`
/// additionally refuses *any* absolute specifier a rule writes, as a bare specifier, before
/// containment is considered at all — so `import '/etc/passwd'` and
/// `import '/inside/the/root/x'` are both refused, and only the first would be refused here.
/// An absolute `.wasm` path that lands inside the rules root is therefore accepted. That is not
/// an escape and nothing about the trust boundary turns on it; it is written down because the
/// two paths are otherwise easy to read as identical, and the next person to compare them
/// should find the difference recorded rather than discover it.
///
/// # One read
///
/// The bytes are read here and carried on [`ComponentRule`]. `metadata` is read from them,
/// `hash_ruleset` folds them and `lanekeep-engine` executes them, so the rule that was
/// described is the rule that runs. Reading three times would let a file that changed in
/// between describe one rule, key another and run a third.
fn describe_components(
    root: &RuleRoot,
    resolved: &[ResolvedRule],
    display: &str,
    limits: Limits,
    artifacts: Option<&Path>,
) -> Result<Vec<Option<Vec<Described>>>, ConfigError> {
    let mut described: Vec<Option<Vec<Described>>> = resolved.iter().map(|_| None).collect();
    if !resolved.iter().any(|rule| rule.reference.is_component()) {
        return Ok(described);
    }

    let fail = |position: usize, detail: String| ConfigError::Rule {
        position: position + 1,
        path: display.to_owned(),
        detail,
    };
    let broken = |detail: String| ConfigError::Shape {
        path: display.to_owned(),
        detail,
    };

    let engine = WasmEngine::new().map_err(|e| broken(e.to_string()))?;
    let mut set = RuleSet::new(&engine).map_err(|e| broken(e.to_string()))?;
    // With the on-disk artifact cache when the caller named a project root, and without one
    // otherwise. A rules root is not a project root — `lanekeep-testkit` anchors one at a
    // temporary fixture directory — so guessing a location to write `.lanekeep/components` into
    // would make loading a config write somewhere nobody asked for. Naming it is
    // `LoadOptions::artifacts`, passed through `load_with`, and the CLI names it.
    //
    // It matters because without one this compiles every component only to throw the
    // compilation away, and the engine compiles the same bytes again at prepare time: ~58 ms per
    // component, on every config load, and config load runs per LSP request, per MCP tool call
    // and per `--watch` iteration. With one, both loads map the same artifact.
    let loader = artifacts.map_or_else(
        lanekeep_wasm::ComponentLoader::without_cache,
        lanekeep_wasm::ComponentLoader::for_project_root,
    );

    // The one clock, started before any guest code runs and shared by the enumeration and the
    // description. Two clocks would give each phase the whole global budget, so a config load
    // could take twice what the user set and report neither overrun.
    let clock = RunClock::start(limits.global_timeout);

    let mut added = Vec::new();
    for (position, rule) in resolved.iter().enumerate() {
        // Whether this reference is a component at all comes first, so a config of TypeScript
        // rules with one component in it does no work per rule that is thrown away. Extracting
        // the two byte sources into `component_bytes` put the serialization above this test for
        // a while, which was a small silent regression on the common shape.
        let Some((origin, bytes)) =
            component_bytes(root, rule).map_err(|detail| fail(position, detail))?
        else {
            continue;
        };

        // `null` for a rule named with no options, which is the world's own shape for it —
        // serialized once here so that every worker's `configure` is handed the same bytes.
        let options = rule
            .options
            .as_ref()
            .map_or_else(|| "null".to_owned(), json::literal);

        let admitted = loader
            .load(&engine, &rule.specifier, bytes.as_slice())
            .map_err(|e| fail(position, e.to_string()))?;

        let ids = hosted_rules(&engine, limits, &clock, &admitted)
            .map_err(|e| fail(position, e.to_string()))?;

        // A component hosting nothing is a configured rule that can never report, which is the
        // failure this tool exists not to produce — and it is silent, because an empty list
        // reads downstream exactly like a reference nobody wrote. Refused where the reference
        // is, so the diagnostic names the entry.
        if ids.is_empty() {
            return Err(fail(
                position,
                format!(
                    "`{}` is a component that hosts no rules — there is nothing for this entry \
                     to run",
                    rule.specifier
                ),
            ));
        }

        for (index, id) in ids.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                fail(
                    position,
                    format!(
                        "`{}` lists more rules than an index can name",
                        rule.specifier
                    ),
                )
            })?;
            // The rule's own id rather than the specifier, because a slot's name is what a
            // diagnostic shows a reader and one specifier now stands for several rules.
            let slot = set
                .add(id, &admitted, index, options.clone())
                .map_err(|e| fail(position, e.to_string()))?;

            added.push((
                position,
                slot,
                ComponentRule {
                    path: origin.clone(),
                    index,
                    options: options.clone(),
                    // An `Arc` clone: the rules of one component share the read, which is what
                    // makes "read once" per reference rather than per rule.
                    bytes: bytes.clone(),
                    // The one constructor whose output `build` folds into `ruleset_hash` —
                    // see the field.
                    counted_in_ruleset_hash: true,
                },
            ));
        }
    }

    let mut runtime = WasmRuntime::for_rules(engine, std::sync::Arc::new(set), limits, clock);

    for (position, slot, component) in added {
        let metadata = runtime
            .metadata(slot)
            .map_err(|e| fail(position, e.to_string()))?;
        let has_check = runtime
            .has_check(slot)
            .map_err(|e| fail(position, e.to_string()))?;
        let has_reduce = runtime
            .has_reduce(slot)
            .map_err(|e| fail(position, e.to_string()))?;

        if let Some(entry) = described.get_mut(position) {
            entry.get_or_insert_with(Vec::new).push(Described {
                raw: raw_rule_from(metadata, has_check, has_reduce),
                component,
            });
        }
    }

    Ok(described)
}

/// Which rules a component hosts, by id, in the order it lists them.
///
/// **The one question that has to be asked before a rule set can be built.** `RuleSet::add`
/// takes an index into this list and cannot discover one for itself: `rules` is an export, so
/// asking needs a store and an instance, and a rule set deliberately holds neither.
///
/// A runtime of its own, built and dropped here. Two things follow. The instance is transient,
/// so the enumeration does not leave one resident per component beside the description's; and
/// this store is not the store the description runs in, so a component that traps while being
/// enumerated poisons nothing that outlives the failure — which costs nothing either way, since
/// every failure here aborts the load.
///
/// The clock is the caller's rather than a fresh one, so the global budget covers the
/// enumeration and the description together.
///
/// # Errors
///
/// [`lanekeep_wasm::WasmError`] if the world cannot be linked, the component cannot be
/// instantiated under the run's limits, or the guest traps while listing its rules.
fn hosted_rules(
    engine: &std::sync::Arc<WasmEngine>,
    limits: Limits,
    clock: &std::sync::Arc<RunClock>,
    admitted: &lanekeep_wasm::Loaded,
) -> Result<Vec<String>, lanekeep_wasm::WasmError> {
    let mut probe = WasmRuntime::new(
        std::sync::Arc::clone(engine),
        limits,
        std::sync::Arc::clone(clock),
    )?;
    let instance = probe.instantiate(admitted)?;
    probe.call_rules(&instance)
}

/// Where one reference's component bytes come from, or `None` if it names no component.
///
/// **The two sources of a component, in one place.** A built-in is embedded in this binary and a
/// project rule is a file inside the rules root, and everything downstream — admission, the rule
/// set, `metadata`, `ruleset_hash`, execution — treats them identically from here on. Keeping the
/// two arms together is what makes that reading true rather than approximately true: a difference
/// between them has to be written in this function, where it can be seen.
///
/// The first element is provenance for [`ComponentRule::path`] — a canonical path for a file, and
/// the `lanekeep/<name>` specifier for a built-in, which is relative and so can never collide
/// with one.
///
/// # A loose `.wasm` is reachable and is not a supported interface
///
/// The file arm below means a `lanekeep.json` naming `./rules/mine.wasm` loads and runs it, and
/// the containment tests beside it are real. It is nonetheless **not** a documented feature:
/// `schema/lanekeep.schema.json` describes built-ins and `./path.ts` only, and
/// `docs/authoring-rust-rules.md` is about the built-ins in this repository rather than about a
/// project shipping its own component.
///
/// That is a decision rather than an oversight, taken because supporting it means promising
/// something not yet true. A third-party component binds against `crates/lanekeep-wasm/wit`,
/// whose bytes are a *cache key* and not a stability promise — it changes without ceremony, and
/// this branch changed it twice — so a rule built against one lanekeep would silently target a
/// world the next one does not have. Advertising the path before there is a versioned world and
/// a published authoring story would be committing to an ABI nothing currently keeps.
///
/// The arm stays because built-ins and fixtures reach it by the same route, and narrowing it to
/// built-ins would put a difference between the two sources back into a function whose whole
/// purpose is that there is not one. Anyone deciding to support it should add the schema entry
/// and the authoring documentation in that change, and say what the world's stability is.
///
/// # Errors
///
/// Returns the diagnostic detail, without a position: the caller knows which rule this was and
/// wraps it. A built-in that the lookup does not know is *unreachable* while `json::classify`
/// asks the very lookup this reads — the reference is only that variant because the name
/// answered. It is refused rather than assumed away because the two calls are in different
/// crates, and a rules root rebuilt between them without its components would otherwise produce
/// a rule with no `check` rather than an explanation.
fn component_bytes(
    root: &RuleRoot,
    rule: &ResolvedRule,
) -> Result<Option<(PathBuf, ComponentBytes)>, String> {
    match &rule.reference {
        // Embedded in this binary, so there is no path to confine and no file to read — and
        // nothing a project file could shadow, which is the guarantee a built-in module has too.
        RuleReference::BuiltinComponent(name) => {
            let bytes = root.builtin_component(name).ok_or_else(|| {
                format!(
                    "`lanekeep/{name}` was resolved as a built-in component and this build has \
                     no component by that name"
                )
            })?;
            Ok(Some((
                PathBuf::from(format!("lanekeep/{name}")),
                bytes.to_vec().into(),
            )))
        }

        RuleReference::Component(path) => {
            // Confinement before the read, and before anything is compiled or run.
            //
            // The message is this crate's rather than the resolver's, because the resolver's is
            // written for an `import` and says so — "rule modules may only import from within
            // it" names nothing a user who wrote a `.wasm` path would recognize. The *check* is
            // the resolver's, which is the half that must not be duplicated.
            let confined = root.confine(&rule.specifier, path).map_err(|e| match e {
                ResolveError::EscapesRoot { .. } => format!(
                    "`{}` resolves outside the rules root, and a rule component must sit \
                     inside it",
                    rule.specifier
                ),
                ResolveError::Unreadable { detail, .. } => {
                    format!("cannot read `{}`: {detail}", path.display())
                }
                other => other.to_string(),
            })?;

            let bytes: ComponentBytes = std::fs::read(&confined)
                .map_err(|e| format!("cannot read `{}`: {e}", confined.display()))?
                .into();
            Ok(Some((confined, bytes)))
        }

        RuleReference::Builtin(_) | RuleReference::Module(_) => Ok(None),
    }
}

/// A component's own account of itself, in the shape [`build_rule`] validates.
struct Described {
    raw: RawRule,
    component: ComponentRule,
}

/// What a component answered, as the rule declaration the rest of this file already knows how
/// to check.
///
/// Deliberately a [`RawRule`] rather than a `RuleSpec`: converging on the same validation is
/// the point. A component that named an undeclared namespace, an empty query or an unusable
/// card is refused by the code that refuses a TypeScript rule for the same reasons, in the
/// same words.
fn raw_rule_from(
    metadata: lanekeep_wasm::bindings::types::RuleMetadata,
    has_check: bool,
    has_reduce: bool,
) -> RawRule {
    RawRule {
        id: Some(metadata.id),
        language: Some(RawLanguages::Many(metadata.languages)),
        severity: Some(metadata.severity),
        card: Some(RawCard {
            message: Some(metadata.card.message),
            remediation: Some(metadata.card.remediation),
            examples: Some(RawExamples {
                bad: Some(metadata.card.examples.bad),
                good: Some(metadata.card.examples.good),
            }),
        }),
        query: Some(metadata.query),
        gates: Gates {
            path_matches: metadata.gates.path_matches,
            path_not_matches: metadata.gates.path_not_matches,
            file_contains: metadata.gates.file_contains,
            file_not_contains: metadata.gates.file_not_contains,
        },
        timeout: metadata.timeout,
        has_check,
        has_reduce,
    }
}

fn build_rule(
    raw: RawRule,
    position: usize,
    display: &str,
    overrides: &BTreeMap<RuleId, Severity>,
    declared: &BTreeSet<String>,
    component: Option<ComponentRule>,
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

    // Both TypeScript dialects by default, because a rule written for TypeScript is meant for
    // the TypeScript in the project — and in any React codebase most of that lives in `.tsx`,
    // which the TypeScript grammar cannot parse.
    let languages = raw.language.map_or_else(
        || vec!["typescript".to_owned(), "tsx".to_owned()],
        RawLanguages::into_vec,
    );
    // An empty list is not "every language", it is *no file at all* — a rule runs only on a
    // file whose own language it names — and it is silent: the rule loads, matches nothing and
    // reports nothing, which is indistinguishable from the code being clean. The world declares
    // that the host refuses one at load (`crates/lanekeep-wasm/wit/world.wit`); this is that
    // refusal, and it covers a TypeScript rule writing `language: []` for the same reason.
    if languages.is_empty() {
        return Err(fail(format!(
            "`{id}` names no language — a rule runs only on files whose language it names, so \
             an empty list means it can never run"
        )));
    }

    Ok(RuleSpec {
        index: position - 1,
        // Config severity wins over what the rule declares, per §9.
        severity: overrides.get(&id).copied().unwrap_or(declared),
        id,
        languages,
        card,
        query,
        gates: raw.gates,
        timeout: raw.timeout.map(Duration::from_millis),
        has_reduce: raw.has_reduce,
        component,
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
/// under-invalidation in this one — two built-ins are components and every other rule in this
/// tree is a module, so dropping the walk would take almost the whole ruleset out of the cache
/// key. So both are folded, and the module walk leaves when the last module does.
///
/// A component is hashed by its **bytes and not its path**. A resolved component path is
/// absolute, and putting it in would make the key depend on where the checkout sits — a cache
/// invalidated by moving a directory, for nothing. Which component a rule *names* is
/// `hash_config`'s to carry, through the specifier; this hash is about the code.
///
/// # A component is folded once, and each rule of it separately
///
/// **A component hosts a list of rules, so "the code" and "a rule" stopped being the same
/// thing.** Folding a component's bytes once per rule it hosts is not wrong, and it is two
/// other things that are: quadratic in the rule count — four rules on the 12.34 MiB
/// TypeScript component would fold 49 MiB — and unable to tell "two rules of one component"
/// from "one component named twice", because both are the same bytes twice.
///
/// So the fold is in two parts. Every **distinct component** contributes its bytes once, in a
/// fixed order; then every **rule** contributes which of those components it runs in, which of
/// that component's rules it is, and what it was configured with. The first part is the
/// programs, the second is what is being asked of them, and neither describes the other.
///
/// *Distinct* is by **content**: two references to one artifact by different paths are the
/// same program, and one path read twice across a rewrite is two. That is the same relation
/// `lanekeep_wasm::Loaded::identity` expresses as a blake3 digest, realized here by comparing
/// the bytes rather than by digesting them — the bytes are already in hand, and a digest pass
/// costs a walk over megabytes on a path that runs per LSP request, per MCP call and per
/// `--watch` iteration.
///
/// A rule names its component **by position in that sorted list** rather than by repeating its
/// identity. That is what keeps the two parts from being two descriptions of one thing: a
/// position says nothing about the bytes, so the component fold stays the only place the code
/// reaches the key, and `two_components_cannot_run_together_into_one` keeps testing the
/// delimiting it is about rather than being answered by a digest folded elsewhere.
///
/// Duplicates collapse in both parts: naming one component twice, at the same rule and with the
/// same options, is a configuration difference and not a different program.
///
/// # It folds bytes it is handed, and does not go and read them
///
/// **This is the same property the module half has, and it used to be the one thing the
/// component half did not.** `sandbox.loaded_modules()` is what the loader actually consumed,
/// so a module that changed after it was read still hashes as the source that produced the
/// answer. The component half used to take the *paths* and read them again — a second read,
/// several milliseconds after `describe_components` read the same files to ask them what they
/// are, and before `lanekeep-engine` read them a third time to run them. A file that changed
/// in between would describe one rule, key another and execute a third, and nothing would
/// notice. So the bytes arrive on [`ComponentRule`], read once, and this folds those.
///
/// **Absence is therefore no longer representable here, and that is stronger than the marker
/// it replaces rather than weaker.** This used to fold a present/absent byte, so that "the
/// component is missing" and "the component is there" could not hash alike — §8.2's rule that
/// a run which could not read a rule and one that could must not share a key. A component that
/// cannot be read now fails config load outright: there is no `Config`, so there is no key and
/// no run, which is what that rule was protecting against in the first place.
/// `a_component_that_is_not_there_is_refused_by_position` is where that lives now.
///
/// The bytes are still length-prefixed, and that is unrelated to the marker: a `.wasm` is
/// arbitrary binary and can contain whichever byte a separator would be, so without the length
/// two components could concatenate into one byte sequence.
/// `two_components_cannot_run_together_into_one` is what says so.
///
/// # `components` is empty for a TypeScript config, so this half is JSON-only today
///
/// Only a `lanekeep.json` produces a [`RuleReference::Component`], so a TypeScript config
/// builds no [`ComponentRule`] and there are no component bytes to miss. **The day it can name
/// one, this is the branch that silently stops covering them** — and the shape above is what
/// makes that harder to get wrong than it was: the bytes come from the rules that were built,
/// so whoever teaches the TypeScript path to name a component gets the fold for free rather
/// than having to remember a second list.
fn hash_ruleset(sandbox: &Sandbox, components: &[&ComponentRule]) -> Hash {
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

    // The distinct programs, in the order their bytes sort in — which is a fixed order that
    // depends on nothing outside the bytes themselves, so reordering a config's rules is not a
    // different ruleset. The path is not consulted at all, for either the order or the identity:
    // it is absolute, so it would throw a cache away for moving a checkout, and which component
    // a rule *names* is `hash_config`'s through the specifier.
    //
    // **The bytes are in the key because "read once" is per reference, not per path.**
    // `component_bytes` reads once per `ResolvedRule`, and nothing deduplicates `rules`, so a
    // config may legitimately name one file twice — `["./r.wasm", {"rule": "./r.wasm", "options":
    // {…}}]` is how a rule is used bare and configured in the same run. If the file is rewritten
    // between those two reads, two `ComponentRule`s carry one path and different bytes, and both
    // execute what they carry. Those are two programs and this folds both, which is the single
    // claim this whole function exists to make.
    //
    // Keying rather than preventing, deliberately. Caching the first read and reusing it would
    // close the window by changing which bytes the second rule *runs*, which is a semantic change
    // to fix a hashing bug — and it cannot make the read atomic either, since there is no
    // snapshot of a live filesystem to take. Hashing what actually ran is the property that was
    // claimed. In the ordinary case both entries have identical bytes and this collapses them.
    let mut distinct: Vec<&[u8]> = components
        .iter()
        .map(|component| component.bytes.as_slice())
        .collect();
    distinct.sort_unstable();
    distinct.dedup();

    hasher.update(b"components");
    length_prefixed(&mut hasher, &(distinct.len() as u64).to_le_bytes());
    for bytes in &distinct {
        length_prefixed(&mut hasher, bytes);
    }

    // What is being asked of those programs: for each rule, which one it runs in, which of that
    // one's rules it is, and what it was configured with. Sorted and deduplicated for the reason
    // the components are — the order a config lists its rules in is `hash_config`'s, and naming
    // the same rule of the same component twice with the same options is one program either way.
    //
    // The component is named by its position in `distinct` rather than by its bytes or a digest
    // of them, so that this fold says nothing about the code and the fold above stays the only
    // place the code reaches the key. Repeating an identity here would leave the component fold
    // provable-by-accident: the delimiting it exists for would be backed up by a second copy of
    // the same information, and the test that asserts it would pass with the delimiting gone.
    //
    // `Err` from the search is unreachable — every slice searched for came out of the very list
    // being searched — and is folded to its insertion point rather than unwrapped, because a
    // panic on a value derived from a user's config is not something this crate does.
    let mut rules: Vec<(usize, u32, &str)> = components
        .iter()
        .map(|component| {
            let bytes = component.bytes.as_slice();
            let at = match distinct.binary_search(&bytes) {
                Ok(at) | Err(at) => at,
            };
            (at, component.index, component.options.as_str())
        })
        .collect();
    rules.sort_unstable();
    rules.dedup();

    hasher.update(b"rules");
    length_prefixed(&mut hasher, &(rules.len() as u64).to_le_bytes());
    for (component, index, options) in rules {
        // Both fixed-width, so neither needs delimiting from the other or from the options
        // that follow.
        hasher.update(&(component as u64).to_le_bytes());
        hasher.update(&index.to_le_bytes());
        length_prefixed(&mut hasher, options.as_bytes());
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
            load_from(&self.dir, name)
        }

        /// A sandbox over this fixture, with nothing loaded into it.
        ///
        /// For the component half of `ruleset_hash`, whose tests want a fold over bytes rather
        /// than over rules: the files they name are a few bytes long and are not components at
        /// all, which is what lets them assert on separators, ordering and absence without
        /// building a real artifact apiece. Going through `load` would refuse every one of them
        /// long before the hash was reached.
        fn empty_sandbox(&self) -> Sandbox {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox")
        }

        /// Copy one of `lanekeep-wasm`'s committed fixture components into this fixture.
        ///
        /// By path at run time rather than `include_bytes!`, because `lanekeep-wasm` excludes
        /// its whole `tests/` tree from the published package — a compile-time include would
        /// make this crate fail to build for anyone who vendored it, where a copy that is only
        /// reached by a test fails nowhere else.
        fn write_component(&self, at: &str, fixture: &str) {
            let from = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lanekeep-wasm/tests/fixtures")
                .join(format!("{fixture}.wasm"));
            let full = self.dir.join(at);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("creates parent");
            }
            fs::copy(&from, &full).expect("the fixture ships");
        }

        /// A component-backed rule over a file inside this fixture.
        ///
        /// **The bytes are read when this is called, not when the hash is taken**, which is
        /// the property `hash_ruleset` now has and is why every test below that edits a file
        /// calls this again afterwards. Reading at hash time is exactly the bug that shape
        /// removes: the hash would then be over a read nobody else made.
        fn component(&self, name: &str) -> ComponentRule {
            self.component_at(name, 0)
        }

        /// The same, naming one of a multi-rule component's rules.
        ///
        /// Separate from [`Fixture::component`] rather than a parameter on it, because rule `0`
        /// is what every test that is not about the index means, and spelling a `0` at a dozen
        /// call sites would make the index look like something those tests had chosen.
        fn component_at(&self, name: &str, index: u32) -> ComponentRule {
            let path = self.dir.join(name);
            // `expect`, not `unwrap_or_default`: a mistyped name would otherwise become empty
            // bytes, and two tests here compare hashes that would then be equal for the wrong
            // reason — `the_ruleset_hash_ignores_where_a_component_sits` and
            // `..._ignores_the_order_and_the_repetition_of_a_component` both assert *equality*,
            // so they pass vacuously against two empty files. The engine's `backed_by` says the
            // same thing for the same reason.
            let bytes = fs::read(&path).expect("the component file is where the test put it");
            ComponentRule {
                path,
                index,
                options: "null".to_owned(),
                bytes: bytes.into(),
                // Irrelevant to what is under test here — these tests drive `hash_ruleset`
                // directly rather than through `Engine::caching` — but `true` is the honest
                // answer: a real `load` is what every one of these fixtures simulates.
                counted_in_ruleset_hash: true,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Load a config with the rules root at a chosen directory.
    ///
    /// Separate from [`Fixture::load_named`] because the confinement tests need a root that is
    /// *inside* the fixture, so that something the fixture wrote is genuinely outside it.
    fn load_from(dir: &Path, name: &str) -> Result<Config, ConfigError> {
        let root = RuleRoot::new(dir).expect("canonicalizes");
        let sandbox =
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
        load(&sandbox, &root, &dir.join(name))
    }

    /// The same, with a built-in component table installed.
    ///
    /// Separate rather than a parameter on every caller, because "no built-in ships as a
    /// component" is what the rest of this suite means and should keep saying.
    fn load_with_components(
        dir: &Path,
        name: &str,
        components: lanekeep_js::BuiltinComponent,
    ) -> Result<Config, ConfigError> {
        let root = RuleRoot::new(dir)
            .expect("canonicalizes")
            .with_builtin_components(components);
        let sandbox =
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
        load(&sandbox, &root, &dir.join(name))
    }

    /// The `metadata` fixture's bytes, served as though they were embedded in the binary.
    ///
    /// Read at run time rather than `include_bytes!`, for the reason [`Fixture::write_component`]
    /// records: `lanekeep-wasm` excludes its whole `tests/` tree from the published package, and
    /// a compile-time include would put a path that does not exist for a vendored checkout into
    /// this crate's source. A `OnceLock` is what turns a run-time read into the `&'static [u8]`
    /// a [`lanekeep_js::BuiltinComponent`] has to return.
    fn built_in_component_bytes() -> &'static [u8] {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES.get_or_init(|| {
            fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../lanekeep-wasm/tests/fixtures/metadata.wasm"),
            )
            .expect("the fixture ships")
        })
    }

    /// A built-in table with exactly one component in it, standing in for `lanekeep_rules`.
    ///
    /// A stub rather than the real table: which rules have migrated is not what these tests are
    /// about, and naming one would make the next migration edit assertions unrelated to it.
    fn built_in_components(name: &str) -> Option<&'static [u8]> {
        match name {
            "metadata" => Some(built_in_component_bytes()),
            _ => None,
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

    // --- components -----------------------------------------------------------------

    #[test]
    fn a_component_reference_resolves_to_a_spec_carrying_its_own_metadata() {
        // Every field below comes from the component's own `metadata` export and from
        // nowhere else — there is no config syntax carrying any of it, which is the whole
        // reason the export exists.
        let fixture = Fixture::new("component-metadata", &[]);
        fixture.write_component("rules/metadata.wasm", "metadata");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"include": ["**/*.rs"], "namespaces": ["fixture"],
                "rules": ["./rules/metadata.wasm"]}"#,
        )]);

        let config = fixture
            .load_json()
            .expect("a component reference is resolvable");

        let rule = &config.rules[0];
        assert_eq!(rule.id.to_string(), "fixture/metadata");
        assert_eq!(rule.query, "(call_expression) @call");
        assert_eq!(rule.languages, ["rust"]);
        assert_eq!(rule.card.message, "a fixture");
        assert_eq!(rule.card.remediation, "do the other thing");
        // All four, and the fixture sets all four to different values on purpose. `raw_rule_from`
        // assigns them from a plain struct literal, so a dropped or swapped field is not a type
        // error — asserting two of the four leaves the other two mapped by nothing, and both
        // mutations pass. This is the shape of the Task 1 finding recurring one layer up.
        assert_eq!(rule.gates.path_matches, ["src/**/*.rs"]);
        assert_eq!(rule.gates.path_not_matches, ["**/generated/**"]);
        assert_eq!(rule.gates.file_contains, ["call"]);
        assert_eq!(rule.gates.file_not_contains, ["skip"]);
        assert_eq!(rule.timeout, Some(Duration::from_millis(1500)));
        assert!(
            !rule.has_reduce,
            "the fixture answers `has-reduce` with false, and the config must take that \
             answer rather than assuming one"
        );
        let component = rule
            .component
            .as_ref()
            .expect("the bytes travel with the rule");
        assert_eq!(
            component.bytes.as_slice(),
            fs::read(fixture.dir.join("rules/metadata.wasm"))
                .expect("the fixture is there")
                .as_slice(),
            "the rule carries the component it was described from"
        );
        // This crate is the only one that can truthfully answer this: `describe_components`'s
        // output is exactly what `build` folds into `ruleset_hash`, a few lines below where the
        // rule this test just built came from. `Engine::caching` (`lanekeep-engine`) trusts this
        // flag rather than re-deriving it, so a `false` here would silently take every
        // component-backed run's cache off — and nothing outside this crate can tell, because a
        // hand-built `ComponentRule` looks identical otherwise. Paired with
        // `an_uncounted_component_is_not_counted_in_ruleset_hash` below, this closes both
        // mutants of `ComponentRule::counted_in_ruleset_hash` inside this crate's own suite: this
        // one alone only kills `replace ... with false`, since nothing here is `false` for a
        // mutant hardcoding `true` to disagree with.
        assert!(
            component.counted_in_ruleset_hash(),
            "a component `load` resolved must be counted in `ruleset_hash`"
        );
    }

    /// The two entry points differ in exactly one observable way, and it is worth tens of
    /// milliseconds per config load.
    ///
    /// `load` has nowhere to write, so it compiles each component only to discard the
    /// compilation, and the engine compiles the same bytes again at prepare time.
    /// [`load_with`] given a [`LoadOptions::artifacts`] root leaves a `.cwasm` under
    /// `COMPONENT_CACHE_PATH` that both this load and the engine's own loader map — measured at
    /// ~58 ms per component per load before, and at TypeScript parity after.
    ///
    /// Asserted on the artifact rather than on a duration: a timing assertion on a loaded machine
    /// is a flake, and the file either exists or it does not. Both directions are asserted,
    /// because a change making *every* load write would pass a one-sided test while putting a
    /// cache directory inside every `lanekeep-testkit` fixture — which is the reason `load` does
    /// not do it.
    #[test]
    fn only_a_load_given_a_project_root_caches_what_it_compiled() {
        let files = &[(
            "lanekeep.json",
            r#"{"include": ["**/*.rs"], "namespaces": ["fixture"],
                "rules": ["./rules/metadata.wasm"]}"#,
        )];

        let plain = Fixture::new("artifact-cache-absent", files);
        plain.write_component("rules/metadata.wasm", "metadata");
        plain.load_json().expect("the component resolves");
        assert!(
            !plain.dir.join(lanekeep_wasm::COMPONENT_CACHE_PATH).exists(),
            "`load` names no project root, so it must not write a cache directory into one"
        );

        let cached = Fixture::new("artifact-cache-present", files);
        cached.write_component("rules/metadata.wasm", "metadata");
        let root = RuleRoot::new(&cached.dir).expect("canonicalizes");
        let sandbox =
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
        load_with(
            &sandbox,
            &root,
            &cached.dir.join("lanekeep.json"),
            LoadOptions {
                artifacts: Some(&cached.dir),
                ..LoadOptions::default()
            },
        )
        .expect("the component resolves");

        let artifacts = cached.dir.join(lanekeep_wasm::COMPONENT_CACHE_PATH);
        let written: Vec<_> = fs::read_dir(&artifacts)
            .expect("the cache directory is there")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "cwasm"))
            .collect();
        assert_eq!(
            written.len(),
            1,
            "one component was described, so one artifact should be cached; found {written:?}"
        );
    }

    /// A caller's `--timeout` has to govern config load, because config load runs guest code.
    ///
    /// **Asserts the raise, which is the direction that can fail.** `AGENTS.md` records why: a
    /// test that only *lowers* a budget passes against a budget that is ignored, because the run
    /// completes either way and completion is what such a test asserts. Raising is different —
    /// the un-overridden load must fail first, so the override is the only thing that can make
    /// the second one succeed.
    ///
    /// This is the `--timeout` trap recurring in a phase that did not exist when it was first
    /// found. The flag used to be applied to the `Config` *after* `load` returned, one statement
    /// below a config load that had already instantiated, configured and read `metadata` from
    /// every component under the config file's number. A component whose `configure` overran
    /// failed with a message ending "raise it with `--timeout`", and raising it changed nothing.
    ///
    /// The 50 ms against a burn of roughly a third of a second is a ratio, not a deadline: a
    /// slower machine only makes the breach breach harder, and the raised case has seconds of
    /// room. Both halves go through `load_with` rather than the CLI, because the CLI is where the
    /// bug was and a test that reproduced its structure would inherit it.
    #[test]
    fn a_raised_global_timeout_governs_config_load_and_not_only_the_run() {
        let files: &[(&str, &str)] = &[(
            "lanekeep.json",
            r#"{"include": ["**/*.rs"], "namespaces": ["fixture"],
                "timeouts": {"global": 50},
                "rules": [{"rule": "./rules/metadata.wasm", "options": {"burn": true}}]}"#,
        )];
        let fixture = Fixture::new("config-load-budget", files);
        fixture.write_component("rules/metadata.wasm", "metadata");
        let root = RuleRoot::new(&fixture.dir).expect("canonicalizes");
        let sandbox =
            sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
        let config_path = fixture.dir.join("lanekeep.json");

        // The config's own budget is far below what the fixture spends in `configure`, so the
        // phase breaches. Asserted on the message as well as on the failure, because a fixture
        // that failed to load for some unrelated reason would satisfy `is_err` and would make
        // the raise below prove nothing.
        let breached = load_with(&sandbox, &root, &config_path, LoadOptions::default())
            .expect_err("50 ms is far below what the fixture's `configure` spends");
        let text = breached.to_string();
        assert!(
            text.contains("budget"),
            "the breach must be the budget rather than something incidental, got: {text}"
        );

        // And raising it is what that message tells the user to do.
        load_with(
            &sandbox,
            &root,
            &config_path,
            LoadOptions {
                global_timeout: Some(Duration::from_secs(30)),
                ..LoadOptions::default()
            },
        )
        .expect("a raised budget must reach the phase that breached under the lower one");
    }

    #[test]
    fn a_built_in_that_ships_as_a_component_resolves_without_a_path() {
        // The same claim as the test above, for the reference a *user* writes. `lanekeep init`
        // scaffolds `"lanekeep/<name>"`, and two of the rules that spelling names are compiled
        // components — so this is the shape every real config takes, where a `.wasm` path is
        // the shape a project rule takes.
        let fixture = Fixture::new("builtin-component-load", &[]);
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"include": ["**/*.rs"], "namespaces": ["fixture"],
                "rules": ["lanekeep/metadata"]}"#,
        )]);

        let config = load_with_components(&fixture.dir, "lanekeep.json", built_in_components)
            .expect("a built-in component is resolvable by specifier");

        let rule = &config.rules[0];
        // Everything about the rule is the component's own answer, exactly as for a path
        // reference. Nothing in the config said any of it.
        assert_eq!(rule.id.to_string(), "fixture/metadata");
        assert_eq!(rule.query, "(call_expression) @call");
        assert_eq!(rule.languages, ["rust"]);

        let component = rule
            .component
            .as_ref()
            .expect("a built-in component reaches the engine as a component");
        assert_eq!(
            component.bytes.as_slice(),
            built_in_component_bytes(),
            "the rule carries the embedded bytes it was described from"
        );
        // A specifier, not a path: there is no file, so there is nothing to canonicalize. It is
        // relative, which is what keeps it from ever colliding with a confined path — those are
        // absolute.
        assert_eq!(component.path, PathBuf::from("lanekeep/metadata"));
        assert!(
            !component.path.is_absolute(),
            "a built-in's provenance must not look like a resolved path"
        );
        assert!(
            component.counted_in_ruleset_hash(),
            "a built-in component `load` resolved must be counted in `ruleset_hash`"
        );
    }

    #[test]
    fn the_same_specifier_is_a_module_in_a_build_where_no_component_ships() {
        // The pair, and it is the assertion that makes the one above mean something. With no
        // component table installed, `lanekeep/metadata` is an ordinary built-in module
        // specifier — and no such module ships, so it is refused as a missing rule rather than
        // silently becoming something else. A `classify` that ignored the lookup would pass the
        // test above and fail here.
        let fixture = Fixture::new("builtin-component-absent", &[]);
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"include": ["**/*.rs"], "namespaces": ["fixture"],
                "rules": ["lanekeep/metadata"]}"#,
        )]);

        let error = fixture
            .load_json()
            .expect_err("nothing ships under that name in this build");

        let rendered = error.to_string();
        assert!(
            rendered.contains("lanekeep/metadata"),
            "the refusal has to name the specifier: {rendered}"
        );
    }

    /// The other half of the pair above. `load` is not the only way to build a
    /// `ComponentRule` — `ComponentRule::uncounted` is the door this crate hands an embedder or
    /// a test that attaches a component outside `load` — and it has to answer honestly too, or
    /// a mutant hardcoding `counted_in_ruleset_hash` to `true` would pass every test in this
    /// crate: nothing above ever exercises a value that is genuinely `false`.
    #[test]
    fn an_uncounted_component_is_not_counted_in_ruleset_hash() {
        let component = ComponentRule::uncounted(
            PathBuf::from("rules/mine.wasm"),
            0,
            "null".to_owned(),
            b"\0asm".to_vec(),
        );
        assert!(
            !component.counted_in_ruleset_hash(),
            "bytes nobody hashed must not claim to be counted"
        );
    }

    // --- an empty language list ---------------------------------------------------------
    //
    // A rule runs only on a file whose language it names, so an empty list is not "every
    // language", it is *no file at all* — and silently: the rule loads, matches nothing and
    // reports nothing, which is what a clean codebase looks like. `wit/world.wit` declares
    // that the host refuses one at load; both ways a rule can arrive have to be held to it,
    // and the check is one piece of code in `build_rule` precisely so that they are.

    #[test]
    fn a_typescript_rule_naming_no_language_is_refused() {
        let fixture = Fixture::new(
            "empty-languages-ts",
            &[
                (
                    "rule.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     export default defineRule({\n\
                       id: 'local/silent',\n\
                       language: [],\n\
                       query: '(identifier) @id',\n\
                       card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
                       check(ctx, m) { ctx.report(m.id); },\n\
                     });\n",
                ),
                ("lanekeep.config.ts", &config_with("rules: [rule]")),
            ],
        );

        let error = fixture
            .load_config()
            .expect_err("a rule that can never run must not load");
        let rendered = error.to_string();
        assert!(rendered.contains("local/silent"), "{rendered}");
        assert!(rendered.contains("names no language"), "{rendered}");
    }

    #[test]
    fn a_component_naming_no_language_is_refused() {
        // The same refusal on the other path, driven through the two functions the wasm path
        // uses — `raw_rule_from` turns what a guest answered into a rule declaration, and
        // `build_rule` validates it. It stops short of a real guest for one reason: no
        // committed fixture answers an empty list, and adding a `.wasm` artifact whose only
        // purpose is to be rejected before it ever runs buys nothing this does not.
        let described = Described {
            raw: raw_rule_from(
                lanekeep_wasm::bindings::types::RuleMetadata {
                    id: "fixture/silent".to_owned(),
                    languages: Vec::new(),
                    severity: "error".to_owned(),
                    card: lanekeep_wasm::bindings::types::RuleCard {
                        message: "m".to_owned(),
                        remediation: "r".to_owned(),
                        examples: lanekeep_wasm::bindings::types::RuleExamples {
                            bad: "a".to_owned(),
                            good: "b".to_owned(),
                        },
                    },
                    query: "(call_expression) @call".to_owned(),
                    gates: lanekeep_wasm::bindings::types::RuleGates {
                        path_matches: Vec::new(),
                        path_not_matches: Vec::new(),
                        file_contains: Vec::new(),
                        file_not_contains: Vec::new(),
                    },
                    timeout: None,
                },
                true,
                false,
            ),
            component: ComponentRule {
                path: PathBuf::from("silent.wasm"),
                index: 0,
                options: "null".to_owned(),
                bytes: Vec::new().into(),
                // This test drives `build_rule` directly, below `describe_components` and
                // `hash_ruleset` both — irrelevant to either, so `true` for the same reason
                // `Fixture::component` gives it.
                counted_in_ruleset_hash: true,
            },
        };

        let declared = BTreeSet::from(["fixture".to_owned()]);
        let error = build_rule(
            described.raw,
            1,
            "lanekeep.json",
            &BTreeMap::new(),
            &declared,
            Some(described.component),
        )
        .expect_err("a component that can never run must not load");

        let rendered = error.to_string();
        assert!(rendered.contains("fixture/silent"), "{rendered}");
        assert!(rendered.contains("names no language"), "{rendered}");
    }

    #[test]
    fn a_component_is_held_to_the_same_card_and_query_a_typescript_rule_is() {
        // End to end, through a real guest: `world-shape.wasm` answers `metadata` with an empty
        // card and an empty query, because it is a probe rather than a rule. A component's
        // answers go through `build_rule` exactly as an extracted TypeScript rule's do, so it
        // is refused for the reasons a TypeScript rule would be.
        let fixture = Fixture::new("component-validated", &[]);
        fixture.write_component("rules/probe.wasm", "world-shape");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"], "rules": ["./rules/probe.wasm"]}"#,
        )]);

        let error = fixture
            .load_json()
            .expect_err("a probe is not a usable rule");
        assert!(
            matches!(error, ConfigError::Rule { position: 1, .. }),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("fixture/world-shape"),
            "the component's own id should name it: {error}"
        );
    }

    // --- confinement ------------------------------------------------------------------
    //
    // A rule reference is a string in a config file and a component is *executed*, so where
    // one may point is a trust boundary. The cases below are the sibling's: `crates/
    // lanekeep-js/src/loader.rs` refuses traversal, an absolute path and a symlink out of the
    // root for a module import, and a `.wasm` reference has to be refused for the same reasons
    // — through `RuleRoot::confine`, which is that same check rather than a second one.

    #[test]
    fn a_component_reference_may_not_traverse_out_of_the_rules_root() {
        // Refused whatever is on disk: `secret.wasm` is real and is one directory up. An error
        // that depended on whether the target existed would tell a reader about the filesystem
        // rather than about their config.
        let fixture = Fixture::new("component-traversal", &[]);
        fixture.write_component("secret.wasm", "metadata");
        fs::create_dir_all(fixture.dir.join("project")).expect("creates the inner root");

        for specifier in ["../secret.wasm", "../../secret.wasm", "./../secret.wasm"] {
            fs::write(
                fixture.dir.join("project/lanekeep.json"),
                format!(r#"{{"namespaces": ["fixture"], "rules": ["{specifier}"]}}"#),
            )
            .expect("writes");

            let error = load_from(&fixture.dir.join("project"), "lanekeep.json")
                .expect_err("traversal must not resolve");
            assert!(
                matches!(error, ConfigError::Rule { position: 1, .. }),
                "{specifier} gave {error:?}"
            );
            assert!(
                error.to_string().contains("outside the rules root"),
                "{specifier} gave {error}"
            );
        }
    }

    #[test]
    fn a_component_reference_may_not_be_an_absolute_path() {
        // Built from `temp_dir` rather than written literally: `Path::is_absolute` is
        // platform-specific, so a literal would take a different branch on each platform. What
        // makes this reachable at all is that `Path::join` lets an absolute path replace the
        // base outright, so joining it against the rules root does not confine it.
        let fixture = Fixture::new("component-absolute", &[]);
        fixture.write_component("outside.wasm", "metadata");

        let outside = fixture.dir.join("outside.wasm");
        let inner = fixture.dir.join("project");
        fs::create_dir_all(&inner).expect("creates the inner root");
        // Two platform hazards sit between this path and the check it is here to reach, and
        // both refuse it for a reason that is not confinement.
        //
        // Forward slashes, because `validate_specifier` rejects any specifier containing a
        // backslash — a guard that predates components and exists because a specifier is
        // interpolated into generated JavaScript. A Windows path spelled `C:\Users\...` is
        // therefore refused one layer above `confine`, with a message about quoting. Spelled
        // `C:/Users/...` it is still absolute — Rust accepts either separator on Windows — and
        // it reaches the confinement check this test names. On Unix the replacement is a no-op.
        //
        // Then `serde_json` rather than `format!`, because a backslash also begins an escape
        // inside a JSON string, so an interpolated Windows path makes the config fail to
        // *parse*. Belt and braces: the replacement above already removes them, and encoding
        // properly keeps this true if the path ever carries something else JSON reserves.
        let forward = outside.display().to_string().replace('\\', "/");
        let specifier = serde_json::to_string(&forward).expect("a path is a JSON string");
        fs::write(
            inner.join("lanekeep.json"),
            format!(r#"{{"namespaces": ["fixture"], "rules": [{specifier}]}}"#),
        )
        .expect("writes");

        let error = load_from(&inner, "lanekeep.json").expect_err("an absolute path is refused");
        assert!(
            error.to_string().contains("outside the rules root"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_component_reference_may_not_be_a_symlink_out_of_the_rules_root() {
        // The case a lexical check cannot see, and the reason `confine` canonicalizes rather
        // than only normalizing. `./link.wasm` sits inside the root and looks entirely
        // innocent.
        let fixture = Fixture::new("component-symlink", &[]);
        fixture.write_component("outside.wasm", "metadata");
        let inner = fixture.dir.join("project");
        fs::create_dir_all(&inner).expect("creates the inner root");
        std::os::unix::fs::symlink(fixture.dir.join("outside.wasm"), inner.join("link.wasm"))
            .expect("creates symlink");
        fs::write(
            inner.join("lanekeep.json"),
            r#"{"namespaces": ["fixture"], "rules": ["./link.wasm"]}"#,
        )
        .expect("writes");

        let error = load_from(&inner, "lanekeep.json").expect_err("a symlink out is refused");
        assert!(
            error.to_string().contains("outside the rules root"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_escaping_component_is_refused_before_its_bytes_are_read() {
        // Confinement that happened after the read would already have loaded, compiled and
        // instantiated whatever the reference pointed at — the check would be a report rather
        // than a guard. Pointing at a path that is *unreadable* rather than absent separates
        // the two: read-then-check reports the permission error, check-then-read reports the
        // escape.
        let fixture = Fixture::new("component-escape-before-read", &[]);
        fixture.write_component("outside.wasm", "metadata");
        let outside = fixture.dir.join("outside.wasm");
        fs::set_permissions(
            &outside,
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .expect("makes it unreadable");

        let inner = fixture.dir.join("project");
        fs::create_dir_all(&inner).expect("creates the inner root");
        fs::write(
            inner.join("lanekeep.json"),
            r#"{"namespaces": ["fixture"], "rules": ["../outside.wasm"]}"#,
        )
        .expect("writes");

        let error = load_from(&inner, "lanekeep.json").expect_err("refused");
        let rendered = error.to_string();
        assert!(
            rendered.contains("outside the rules root"),
            "the escape must be what stopped it, not the read: {rendered}"
        );
        assert!(
            !rendered.contains("Permission denied"),
            "nothing may be read before the reference is confined: {rendered}"
        );

        // Left readable, or the fixture's own cleanup cannot remove it.
        fs::set_permissions(
            &outside,
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .expect("restores");
    }

    /// A TypeScript rule sitting after a component still reaches its own handler.
    ///
    /// **The silent failure this is written against.** `RuleSpec::index` is how the engine
    /// reaches a TypeScript handler — it is spelled `__lanekeepConfig.rules[index].check(...)`
    /// — and a component contributes nothing to that array. Numbering the array separately
    /// from the config would leave rule 2 of a mixed config at array position 1: the call
    /// succeeds, the wrong rule's handler runs, and every violation is reported under a
    /// neighbor's id. Nothing errors and nothing looks wrong.
    ///
    /// So the two numberings are one numbering, and this is what says so end to end: the
    /// TypeScript rule's `index` is its position in the config, and the entry module has that
    /// rule at that position.
    #[test]
    fn a_typescript_rule_after_a_component_keeps_its_own_index() {
        let fixture = Fixture::new(
            "component-mixed-order",
            &[("second.ts", &rule("local/second"))],
        );
        fixture.write_component("rules/metadata.wasm", "metadata");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"],
                "rules": ["./rules/metadata.wasm", "./second"]}"#,
        )]);

        let config = fixture.load_json().expect("loads");

        assert_eq!(config.rules[0].id.to_string(), "fixture/metadata");
        assert_eq!(config.rules[0].index, 0);
        assert_eq!(config.rules[1].id.to_string(), "local/second");
        assert_eq!(
            config.rules[1].index, 1,
            "the TypeScript rule's index is its position in the array the engine indexes"
        );
        assert!(config.rules[1].component.is_none());
    }

    /// One reference, one component, and every rule the component hosts.
    ///
    /// **The rule a config names is not the unit a component is.** A component hosts a list —
    /// which is what makes one 12.34 MiB JavaScript engine worth building rules on rather than
    /// one copy per rule — so describing one reference means enumerating it and then describing
    /// each rule by position. A description that stopped at rule 0 would load cleanly and leave
    /// every later rule of the component configured, cached and never run, which looks exactly
    /// like a codebase that is clean.
    #[test]
    fn one_component_describes_every_rule_it_hosts() {
        let fixture = Fixture::new(
            "component-many-rules",
            &[("second.ts", &rule("local/last"))],
        );
        fixture.write_component("rules/two-rules.wasm", "two-rules");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"],
                "rules": ["./rules/two-rules.wasm", "./second"]}"#,
        )]);

        let config = fixture.load_json().expect("loads");

        let ids: Vec<String> = config.rules.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(ids, ["fixture/first", "fixture/second", "local/last"]);

        // Each rule described as itself, not as its neighbor. The fixture's query is the one
        // field that has nothing to do with its configuration, so two rules collapsing into one
        // description shows up here whatever `configure` did.
        assert_eq!(config.rules[0].query, "(call_expression) @0");
        assert_eq!(config.rules[1].query, "(call_expression) @1");

        let first = config.rules[0]
            .component
            .as_ref()
            .expect("a component-backed rule");
        let second = config.rules[1]
            .component
            .as_ref()
            .expect("a component-backed rule");
        assert_eq!((first.index, second.index), (0, 1));
        assert_eq!(
            first.bytes, second.bytes,
            "two rules of one component are one artifact, read once"
        );

        // The entry module has one slot for the reference and none for what it turned out to
        // hold, so both rules carry the reference's own position — and the TypeScript rule
        // after them keeps the position it was written at. Numbering `Config::rules` instead
        // would leave `local/last` reaching the component's placeholder, which is `null`.
        assert_eq!(config.rules[0].index, 0);
        assert_eq!(config.rules[1].index, 0);
        assert_eq!(
            config.rules[2].index, 1,
            "a rule after a multi-rule component still indexes the array the engine indexes"
        );
        assert!(config.rules[2].component.is_none());
    }

    /// And the options a reference carries reach every one of them, before metadata is read.
    ///
    /// A reference names a component, and there is no syntax naming one rule inside it, so a
    /// family of rules shipped in one artifact is configured as a family. The fixture echoes
    /// its `tag` back through `metadata`, which is the one export whose answer is allowed to
    /// depend on `configure` — so this also says the two calls happened in that order, for
    /// each rule rather than for the first.
    #[test]
    fn a_multi_rule_components_options_reach_each_of_its_rules() {
        let fixture = Fixture::new("component-many-options", &[]);
        fixture.write_component("rules/two-rules.wasm", "two-rules");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"],
                "rules": [{"rule": "./rules/two-rules.wasm", "options": {"tag": "alpha"}}]}"#,
        )]);

        let config = fixture.load_json().expect("loads");

        let messages: Vec<&str> = config
            .rules
            .iter()
            .map(|r| r.card.message.as_str())
            .collect();
        assert_eq!(
            messages,
            ["fixture/first tag=alpha", "fixture/second tag=alpha"]
        );
        for spec in &config.rules {
            assert_eq!(
                spec.component
                    .as_ref()
                    .expect("a component-backed rule")
                    .options,
                r#"{"tag":"alpha"}"#
            );
        }
    }

    #[test]
    fn a_component_carries_the_options_it_was_configured_with() {
        // A component cannot close over a host-supplied value, so its options travel with it
        // as data — all the way to every worker's `configure`. A rule named with no options is
        // still configured, with `null`, which is the world's own shape for it.
        let fixture = Fixture::new("component-options", &[]);
        fixture.write_component("rules/metadata.wasm", "metadata");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"],
                "rules": [{"rule": "./rules/metadata.wasm", "options": {"allow": ["a.rs"]}}]}"#,
        )]);

        let config = fixture.load_json().expect("loads");
        let component = config.rules[0]
            .component
            .as_ref()
            .expect("a component-backed rule");
        assert_eq!(component.options, r#"{"allow":["a.rs"]}"#);

        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"], "rules": ["./rules/metadata.wasm"]}"#,
        )]);
        let bare = fixture.load_json().expect("loads");
        assert_eq!(
            bare.rules[0]
                .component
                .as_ref()
                .expect("a component-backed rule")
                .options,
            "null"
        );
    }

    #[test]
    fn a_component_that_refuses_its_options_is_refused_at_load() {
        // A misconfigured rule is not a rule that misbehaved, and the difference is what the
        // user can do about it. The guest's own message has to survive to the diagnostic,
        // because it is the only part that names what was wrong with the configuration.
        let fixture = Fixture::new("component-bad-options", &[]);
        fixture.write_component("rules/metadata.wasm", "metadata");
        fixture.write_all(&[(
            "lanekeep.json",
            r#"{"namespaces": ["fixture"],
                "rules": [{"rule": "./rules/metadata.wasm", "options": [1, 2]}]}"#,
        )]);

        let error = fixture
            .load_json()
            .expect_err("the fixture refuses an array");

        assert!(
            matches!(error, ConfigError::Rule { position: 1, .. }),
            "the diagnostic should name which entry: {error:?}"
        );
        assert!(
            error.to_string().contains("expected an object"),
            "the guest's own message should survive: {error}"
        );
    }

    /// A component that cannot be read fails the load, so it never reaches a hash.
    ///
    /// **This is where §8.2's "absence is a dependency" went, and it is stronger here.**
    /// `ruleset_hash` used to fold a present/absent marker per component, so that a missing
    /// one and a present one could not share a cache key; it folds bytes that were already
    /// read now, and cannot see absence at all. It does not need to: the run whose key would
    /// have been wrong does not happen. A missing component is refused before a `Config`
    /// exists, naming which entry, so there is nothing to serve a stale answer to.
    #[test]
    fn a_component_that_is_not_there_is_refused_by_position() {
        let fixture = Fixture::new(
            "component-missing",
            &[
                ("first.ts", &rule("local/first")),
                (
                    "lanekeep.json",
                    r#"{"rules": ["./first", "./rules/gone.wasm"]}"#,
                ),
            ],
        );

        let error = fixture.load_json().expect_err("there are no bytes to run");
        assert!(
            matches!(error, ConfigError::Rule { position: 2, .. }),
            "the diagnostic should name which entry: {error:?}"
        );
        assert!(error.to_string().contains("gone.wasm"), "{error}");
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

        let before = hash_ruleset(&sandbox, &[&fixture.component("mine.wasm")]);
        fixture.write_all(&[("mine.wasm", "\u{0}asm-two")]);
        let after = hash_ruleset(&sandbox, &[&fixture.component("mine.wasm")]);

        assert_ne!(
            hex(&before),
            hex(&after),
            "rebuilding a rule component must invalidate its cached results"
        );
    }

    #[test]
    fn two_rules_from_one_component_fold_its_bytes_once() {
        // Same component, two rules. The bytes must reach ruleset_hash once, or the hash is
        // quadratic in a component's rule count and cannot tell "one component, two rules"
        // from "one component named twice".
        let fixture = Fixture::new("component-two-rules", &[("a.wasm", "\u{0}asm-two-rules")]);
        let sandbox = fixture.empty_sandbox();

        let one = hash_ruleset(
            &sandbox,
            &[
                &fixture.component_at("a.wasm", 0),
                &fixture.component_at("a.wasm", 1),
            ],
        );
        let twice = hash_ruleset(
            &sandbox,
            &[
                &fixture.component_at("a.wasm", 0),
                &fixture.component_at("a.wasm", 0),
            ],
        );
        assert_ne!(
            one, twice,
            "distinct rule indices must not hash the same as the same index twice"
        );

        // And the once-ness the name is about, which the inequality above does not reach: it
        // holds whether the bytes were folded once or twice, so on its own this test survived
        // `distinct.dedup()` being deleted.
        //
        // **Repetition is the only lever that can observe how many times a component was
        // folded, and that is forced by the encoding rather than chosen.** The two halves cannot
        // be compared separately — a hash has no halves — so seeing the component fold's
        // multiplicity means holding the rule fold still while it moves, and the rule fold
        // encodes the rule count. Listing a rule a second time is the one edit that changes
        // nothing there, because rules deduplicate too. So: a component named three times for
        // two rules is folded exactly as it is when it is named twice.
        let listed_again = hash_ruleset(
            &sandbox,
            &[
                &fixture.component_at("a.wasm", 0),
                &fixture.component_at("a.wasm", 1),
                &fixture.component_at("a.wasm", 0),
            ],
        );
        assert_eq!(
            one, listed_again,
            "a component's bytes must reach the fold once however many times it is listed"
        );
    }

    /// A rule index means nothing on its own, so the fold has to say which component it is in.
    ///
    /// The half `two_rules_from_one_component_fold_its_bytes_once` cannot reach. Both rulesets
    /// below hold the same two components and the same two indices — the deal is swapped — so
    /// the component fold is identical between them and a rule fold recording only the index
    /// would sort to the same pair. They run different code: one asks `a` for its second rule
    /// and `b` for its first, the other the reverse.
    #[test]
    fn a_rule_is_folded_against_the_component_it_runs_in() {
        let fixture = Fixture::new(
            "component-rule-pairing",
            &[("a.wasm", "\u{0}asm-a"), ("b.wasm", "\u{0}asm-b")],
        );
        let sandbox = fixture.empty_sandbox();

        let dealt = hash_ruleset(
            &sandbox,
            &[
                &fixture.component_at("a.wasm", 0),
                &fixture.component_at("b.wasm", 1),
            ],
        );
        let swapped = hash_ruleset(
            &sandbox,
            &[
                &fixture.component_at("a.wasm", 1),
                &fixture.component_at("b.wasm", 0),
            ],
        );

        assert_ne!(
            hex(&dealt),
            hex(&swapped),
            "which component a rule index belongs to is part of the ruleset"
        );
    }

    /// And the options a rule was configured with, which decide what a factory rule *is*.
    ///
    /// `hash_config` folds a JSON config's options too, through `resolved`. That is not this
    /// claim: `resolved` is empty for a TypeScript config, and the day that path can name a
    /// component the options would reach no key at all. The code a component runs includes what
    /// it was configured to be, so it is folded where the code is.
    #[test]
    fn the_ruleset_hash_covers_the_options_a_component_was_configured_with() {
        let fixture = Fixture::new("component-options-hash", &[("a.wasm", "\u{0}asm-a")]);
        let sandbox = fixture.empty_sandbox();

        let bare = fixture.component("a.wasm");
        let mut configured = fixture.component("a.wasm");
        configured.options = r#"{"limit":1}"#.to_owned();

        assert_ne!(
            hex(&hash_ruleset(&sandbox, &[&bare])),
            hex(&hash_ruleset(&sandbox, &[&configured])),
            "a component configured differently is a different ruleset"
        );
    }

    #[test]
    fn two_components_cannot_run_together_into_one() {
        // The reason a component's bytes are length-prefixed, and now the only thing that says
        // so — the length is the whole of the delimiting.
        //
        // A module's source is text and its separator is a NUL. A component is arbitrary
        // binary, so there is no byte available to separate one from the next: whichever were
        // chosen could appear inside a component. Without the length, these two rulesets are
        // genuinely different and fold to the identical byte sequence, under an identical
        // component count:
        //
        //   A:  'A' 'A' | 'B' 'B' 'C' 'C'      a = "AA",   b = "BBCC"
        //   B:  'A' 'A' 'B' 'B' | 'C' 'C'      a = "AABB", b = "CC"
        //
        // **The data used to carry a `\x01` and stopped discriminating when it was no longer
        // needed.** The bytes were built around the old present/absent marker acting as the
        // delimiter, so removing the marker made the two rows genuinely different sequences and
        // this test passed with `length_prefixed` deleted. Concatenation is the property; the
        // data has to be a real collision under it.
        //
        // **Re-derived once more when `hash_ruleset` split into a component fold and a rule
        // fold**, and it survived unchanged — which is a fact about the encoding that was
        // chosen and is not a reason to skip the check. The components are ordered by their
        // *bytes* now rather than by their paths, and `"AA" < "BBCC"` exactly as
        // `a.wasm < b.wasm` did, so both rows still fold to `AABBCC`. Ordering them by a digest
        // instead would have made each row's order a coin flip and this collision a one-in-four
        // accident. What the rule fold contributes is identical between the rows — both are two
        // rules, at index 0, with `null` options, in components 0 and 1 — so the length prefix
        // is still the only thing telling the rows apart. Verified by deleting it and watching
        // this test fail, which is the only form the check has.
        //
        // Two different rulesets sharing a cache key is the one failure `docs/architecture.md`
        // §8.1 exists to prevent, so it is asserted here rather than left to the fact that
        // nothing writes a `.wasm` by hand.
        let fixture = Fixture::new("component-run-together", &[("a.wasm", ""), ("b.wasm", "")]);
        let sandbox = fixture.empty_sandbox();

        fixture.write_all(&[("a.wasm", "AA"), ("b.wasm", "BBCC")]);
        let split_early = hash_ruleset(
            &sandbox,
            &[&fixture.component("a.wasm"), &fixture.component("b.wasm")],
        );

        fixture.write_all(&[("a.wasm", "AABB"), ("b.wasm", "CC")]);
        let split_late = hash_ruleset(
            &sandbox,
            &[&fixture.component("a.wasm"), &fixture.component("b.wasm")],
        );

        assert_ne!(
            hex(&split_early),
            hex(&split_late),
            "two components must not be able to concatenate into one byte sequence — the \
             length is the only thing delimiting them, because any separator byte can appear \
             inside a component"
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
            hex(&hash_ruleset(&sandbox, &[&fixture.component("a.wasm")])),
            hex(&hash_ruleset(
                &sandbox,
                &[&fixture.component("nested/b.wasm")]
            )),
            "the same component bytes are the same ruleset wherever they sit"
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

        let canonical = hex(&hash_ruleset(&sandbox, &[&one, &two]));
        assert_eq!(
            canonical,
            hex(&hash_ruleset(&sandbox, &[&two, &one])),
            "reordering two components is not a different ruleset"
        );
        assert_eq!(
            canonical,
            hex(&hash_ruleset(&sandbox, &[&one, &two, &one])),
            "naming one component twice is not a different ruleset"
        );
    }

    /// One path can carry two byte sequences, and both have to reach the key.
    ///
    /// `component_bytes` reads once per `ResolvedRule` and nothing deduplicates `rules`, so a
    /// config naming one file twice — bare in one entry and with options in another — reads it
    /// twice. A rewrite between those reads produces exactly the pair below, and both rules go on
    /// to execute the bytes they carry. Deduplicating on the path alone kept one of them
    /// arbitrarily, so the key described a ruleset that was not running.
    ///
    /// The window is microseconds and the trigger is exotic. It is asserted anyway because the
    /// claim it falsifies — the bytes hashed are the bytes that run — is the one the component
    /// half of `ruleset_hash` exists to make, and a claim with one shape that breaks it is not
    /// quite the claim.
    #[test]
    fn one_path_with_two_byte_sequences_reaches_the_ruleset_hash_as_both() {
        let fixture = Fixture::new("component-torn-read", &[("r.wasm", "\u{0}asm-before")]);
        let sandbox = fixture.empty_sandbox();

        // The first reference's read.
        let before = fixture.component("r.wasm");
        // The file is rewritten, and the second reference reads what is there now. Both carry
        // the same path, because it is the same file.
        fixture.write_all(&[("r.wasm", "\u{0}asm-after")]);
        let after = fixture.component("r.wasm");
        assert_eq!(
            before.path, after.path,
            "the fixture is one file, read twice"
        );
        assert_ne!(
            before.bytes.as_slice(),
            after.bytes.as_slice(),
            "the rewrite is what makes this pair interesting"
        );

        assert_ne!(
            hex(&hash_ruleset(&sandbox, &[&before, &after])),
            hex(&hash_ruleset(&sandbox, &[&before, &before])),
            "a run executing two different components must not key as one executing the first \
             twice — deduplicating on the path alone made these equal"
        );
        assert_ne!(
            hex(&hash_ruleset(&sandbox, &[&before, &after])),
            hex(&hash_ruleset(&sandbox, &[&after, &after])),
            "nor as one executing the second twice"
        );
    }

    #[test]
    fn the_ruleset_hash_still_covers_modules_when_a_component_is_present() {
        // The deviation this change makes from its own plan, asserted rather than described.
        // The plan said the component fold *replaces* the module walk; two built-ins are
        // components and every other rule in this tree is a module, so that would have taken
        // almost the whole ruleset out of the cache key.
        let files: &[(&str, &str)] = &[
            ("rule.ts", &rule("local/example")),
            ("lanekeep.config.ts", ""),
        ];
        let fixture = Fixture::new("component-and-module", files);
        fixture.write_all(&[("lanekeep.config.ts", &config_with("rules: [rule]"))]);
        fixture.write_all(&[("mine.wasm", "\u{0}asm")]);

        let mine = fixture.component("mine.wasm");
        let root = RuleRoot::new(&fixture.dir).expect("canonicalizes");
        let hash_after_loading = |source: &str| {
            fixture.write_all(&[("rule.ts", source)]);
            let sandbox =
                sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript)).expect("sandbox");
            evaluate_into(&sandbox, &root, &fixture.dir.join("lanekeep.config.ts"))
                .expect("evaluates");
            hash_ruleset(&sandbox, &[&mine])
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
