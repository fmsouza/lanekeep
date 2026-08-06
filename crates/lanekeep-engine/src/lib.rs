//! Rule execution engine for lanekeep.
//!
//! Discovery, gates, parsing, query matching and handler invocation, run over a corpus.
//!
//! # Why this is not in `lanekeep-core`
//!
//! Architecture §3 places the walker in `lanekeep-core`. That does not work: running rules
//! requires the sandbox, and the sandbox is built *on* core — putting the walker there
//! would make `lanekeep-core` and `lanekeep-js` mutually dependent.
//!
//! It cannot live in `lanekeep-cli` either, because `lanekeep-testkit` has to run rules
//! too, and a test harness that reached into the binary crate would be a worse coupling
//! than this one. So the walker sits above the sandbox and below both consumers.
//!
//! Core keeps what it always had: the types, discovery, gates, and the ordering contract.
//!
//! # The shape of a run
//!
//! ```text
//! discover paths (sorted)
//!   └─> for each file, in parallel:
//!         path gates ──reject──> skip without reading
//!         read bytes
//!         content gates ──reject──> skip without parsing
//!         parse once, shared by every rule targeting the file
//!         for each admitted rule: match its query, invoke the handler per match
//!   └─> sort violations
//! ```
//!
//! One parse per file, not per rule. Parsing is the dominant cost, and a file with twenty
//! applicable rules must not pay it twenty times.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lanekeep_cache::{CacheKey, Entry as CacheEntry, GrammarKey, RunKey, Store};
use lanekeep_config::{Config, ConfigError, RuleSpec};
use lanekeep_core::suppression::{self, Date, Suppressions};
use lanekeep_core::{
    CompiledGates, Discovery, DiscoveryError, Fact, FilePath, Location, Position, RuleId, Severity,
    TrackedRead, Violation,
};
use lanekeep_js::{
    FileAccess, HOST_API_VERSION, HostContext, Limits, ReduceContext, ReduceFact, RuleRoot,
    RunClock, Sandbox, SandboxError,
};
use lanekeep_lang::{Language, LanguageRegistry};
use lanekeep_query::{CompileError, CompiledQuery};
use lanekeep_wasm::bindings::types;
use lanekeep_wasm::host::{CheckContext, ReduceContext as ComponentReduceContext};
use lanekeep_wasm::{
    ComponentLoader, EXTERNAL_BINDINGS, ExternalBinding, Resource, RuleSet, RuleSlot, WasmEngine,
    WasmError, WasmRuntime,
};
use rayon::prelude::*;
use thiserror::Error;

/// Why a run could not complete.
///
/// Every variant aborts the run. A checker that could not finish must not be mistaken for
/// one that found nothing — see architecture §6.8.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RunError {
    /// Discovery could not run.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    /// A rule's query does not compile.
    #[error("rule `{rule}` has an invalid query\n{detail}")]
    Query {
        /// Which rule.
        rule: String,
        /// The rendered compile error.
        detail: String,
    },

    /// A rule names a language nothing provides.
    #[error("rule `{rule}` targets unknown language `{language}`\n  known languages: {known}")]
    UnknownLanguage {
        /// Which rule.
        rule: String,
        /// The language as written.
        language: String,
        /// What is available.
        known: String,
    },

    /// The WebAssembly runtime could not be described, so a run cannot be keyed against it.
    ///
    /// Not about any rule. A cached result is only valid for the compilation environment it
    /// was produced under, and that environment is read off an engine built from
    /// `lanekeep_wasm`'s one configuration — so a host where `wasmtime` cannot realize it is a
    /// host where no result can be filed under a key that means anything. Guessing a value
    /// instead would put entries under a key describing nothing, which is the one failure
    /// `docs/architecture.md` §8.1 is arranged against.
    ///
    /// **What this costs, stated plainly: a host where `wasmtime` cannot build an engine can
    /// now run no rules at all — including a ruleset that is entirely TypeScript and needs no
    /// WebAssembly whatever.** That is a real reduction in reach for no benefit until a
    /// component actually executes, and it is the price of keying every run against the
    /// environment rather than only the runs that use it. The alternative is a sentinel for
    /// "no wasm here", which is a second code path through the cache key whose correctness
    /// nothing would exercise until the first component arrived. Worth revisiting if this ever
    /// fires on a real host; nothing has seen it fire, because the configuration is three
    /// tunables on a supported target.
    #[error(
        "the WebAssembly runtime could not be configured on this host\n  {detail}\n  \
         this is a broken build rather than anything about a rule"
    )]
    WasmRuntime {
        /// What `wasmtime` said.
        detail: String,
    },

    /// A rule's gates are malformed.
    #[error("rule `{rule}` has invalid gates: {detail}")]
    Gates {
        /// Which rule.
        rule: String,
        /// What is wrong.
        detail: String,
    },

    /// A rule's component could not be loaded, or could not be linked against the host world.
    ///
    /// Separate from [`RunError::Rule`], which is a rule that ran and failed. This one never
    /// ran: its bytes are missing, its import list reaches for something the sandbox does not
    /// grant, or its exports do not satisfy `lanekeep:host`'s `rule` world. All three are
    /// properties of the artifact rather than of any file, which is why there is no `file`
    /// field, and all three are found before a file is read.
    #[error("rule `{rule}` could not load its component\n{detail}")]
    Component {
        /// Which rule.
        rule: String,
        /// What the component runtime said.
        detail: String,
    },

    /// The run's wall-clock budget was spent, noticed between one file and the next.
    ///
    /// **The only limit breach that names no rule and no file, because it is about neither.**
    /// Both engines already report a spent run budget from inside a handler — QuickJS from its
    /// interrupt handler, wasmtime from an epoch check compiled into guest code — and those
    /// arrive as [`RunError::Rule`], carrying whichever rule happened to be executing. That is
    /// the right shape for a breach a rule was at least present for. It is the wrong shape for
    /// this one: nothing was executing, so there is no culprit to name and naming one would
    /// send a reader to a rule that is not the problem.
    ///
    /// The wording is deliberately the same as both engines', because the user-facing fact is
    /// the same and which mechanism noticed is lanekeep's business rather than theirs.
    #[error(
        "the run exceeded its {budget:?} budget after {elapsed:?}\n  \
         no single rule necessarily misbehaved — the total simply ran too long\n  \
         raise it with `--timeout`, or narrow what is being checked"
    )]
    RunTimeout {
        /// The global budget.
        budget: Duration,
        /// How long the run had actually been going.
        elapsed: Duration,
    },

    /// The sandbox failed, including on a breached budget.
    #[error("rule `{rule}` failed on `{file}`\n{detail}")]
    Rule {
        /// Which rule.
        rule: String,
        /// Which file it was running against.
        file: String,
        /// The sandbox's account of it.
        detail: String,
    },

    /// A worker could not be set up.
    #[error("could not start a worker: {detail}")]
    Worker {
        /// What went wrong.
        detail: String,
    },
}

/// A rule prepared for execution: metadata plus everything compiled.
///
/// The query is compiled once per language the rule targets, because a query is compiled
/// against a grammar and the grammars differ. Which one a given file uses is decided by the
/// file, not by the rule — see [`Prepared::for_language`].
struct Prepared {
    /// This rule's position in [`Engine::rules`].
    ///
    /// Carried on the rule rather than paired with it in the admitted list, because that
    /// list is rebuilt for every file — on a warm run too, before the cache is consulted —
    /// and widening its elements to a tuple measured about 5 ms over the §15 corpus.
    index: usize,
    spec: RuleSpec,
    gates: CompiledGates,
    /// Compiled query per language, in the order the rule declared them.
    compiled: Vec<(Arc<dyn Language>, CompiledQuery)>,
    /// Where this rule's handlers live in the run's [`RuleSet`], or `None` for a TypeScript
    /// rule executed through the QuickJS sandbox.
    ///
    /// **This is the whole of the dispatch decision**, and it is read off
    /// [`RuleSpec::component`] rather than derived from anything else, so a rule that names a
    /// component runs as a component and a rule that does not cannot accidentally become one.
    /// Both kinds coexist in one run over one corpus, which is what the second path exists for:
    /// every built-in and every self-check rule is TypeScript today, so replacing the first
    /// path rather than adding beside it would leave nothing able to run.
    slot: Option<RuleSlot>,
}

impl Prepared {
    /// The grammar and query to use for a file of the given language, or `None` when this
    /// rule does not target it — in which case the rule does not run on that file at all.
    ///
    /// Running it anyway is what the old behavior did, and it does not fail loudly: the file
    /// parses into a tree of `ERROR` nodes and every query quietly matches nothing.
    fn for_language(&self, id: &str) -> Option<&(Arc<dyn Language>, CompiledQuery)> {
        self.compiled
            .iter()
            .find(|(language, _)| language.id().as_str() == id)
    }
}

/// The file a rule is about to run against, and the tree every rule on it shares.
struct FileUnderCheck<'a> {
    path: &'a FilePath,
    source: &'a str,
    tree: &'a tree_sitter::Tree,
    /// The grammar that parsed it — the file's, never a rule's.
    ///
    /// Carried on the file rather than looked up per rule because the component engine needs
    /// it once per *file*: `lanekeep_wasm::host::CheckContext` is built per file and requires a
    /// grammar at construction, which is how that crate makes "a context that cannot compile a
    /// scoped query" an unrepresentable state rather than an answer.
    language: &'a Arc<dyn Language>,
}

/// Walk the tree for one component rule alone, through the context's own arena.
///
/// The fallback path: no combined query for this language, or `--profile` asked for the
/// per-rule split. Collected through the *context's* arena rather than a temporary one so a
/// capture path is taken from the same tree that will later intern it into a handle.
fn walk_for(host: &CheckContext, query: &CompiledQuery, source: &str) -> RuleMatches {
    let arena = host.arena();
    let mut found: RuleMatches = Vec::new();
    query.for_each_match(arena.tree(), source.as_bytes(), |m| {
        let captures = m
            .captures
            .iter()
            .filter_map(|(name, node)| arena.path_of(*node).map(|path| ((*name).to_owned(), path)))
            .collect();
        found.push(captures);
    });
    found
}

/// What every component rule on one file shares.
///
/// The two things that are per *file* rather than per rule, carried together because they are
/// the same decision: the read memo, and the context the arena and the query cache live in.
/// Splitting them would let one be built per rule while the other was not, which is the
/// disagreement the sharing exists to prevent.
struct ComponentPass<'a> {
    files: &'a Arc<FileAccess>,
    /// Opened by the first component rule with a match, and taken back when the file is done.
    context: &'a mut Option<Resource<CheckContext>>,
}

/// One match's captures: the capture name, and a structural path to the node it bound.
///
/// A path rather than a node because a node borrows its tree, and these outlive the borrow
/// — see `NodeArena::path_of`. It being structural is also what lets one traversal serve
/// every rule: the path interns correctly into any arena over the same tree.
type MatchCaptures = Vec<(String, Vec<u32>)>;

/// Every match one rule found in one file.
type RuleMatches = Vec<MatchCaptures>;

/// Matches from one traversal, indexed by position in [`Engine::rules`].
type MatchesByRule = Vec<RuleMatches>;

/// One language's patterns, accumulated across rules before anything is compiled.
struct Concatenation {
    language: Arc<dyn Language>,
    source: String,
    owners: Vec<usize>,
}

/// Every rule's query for one language, compiled as a single multi-pattern query.
///
/// Twenty rules used to mean twenty `QueryCursor` walks of the same tree. tree-sitter is
/// built to evaluate many patterns in one traversal — that is what a `highlights.scm` is —
/// and doing it that way measured 20× faster over the §15 corpus at identical capture
/// counts. It is the single biggest cost left in a cold run.
///
/// Correctness rests on two facts. `pattern_index` says which pattern produced a match, so
/// matches can be handed back to the rule that asked for them. And a capture path is a walk
/// of child indices from the root — see `NodeArena::path_of` — so it is a property of the
/// tree's *shape*, not of any one arena, and a path collected here interns correctly into
/// every rule's own arena afterwards.
struct CombinedQuery {
    language: Arc<dyn Language>,
    source: String,
    /// `owners[pattern_index]` is the index into [`Engine::rules`] that contributed it.
    ///
    /// A rule's query source may hold several patterns, so this is not one entry per rule.
    owners: Vec<usize>,
    /// Compiled on first use, and never on a run that has no use for it.
    ///
    /// Compiling eagerly cost a warm run 26 ms — every file was a cache hit, no query ran,
    /// and the whole compilation was thrown away. Warm is the scenario in the inner loop
    /// and the one with the tightest budget, so paying for cold there is the wrong trade.
    compiled: std::sync::OnceLock<Option<CompiledQuery>>,
}

impl CombinedQuery {
    /// The compiled query, or `None` if the concatenation will not serve.
    ///
    /// `None` sends the file down the per-rule path: slower, never wrong. Every part was
    /// already compiled individually at preparation, which is where a broken query is
    /// reported against the rule that owns it, so this only catches a concatenation
    /// rejected for a reason no single pattern was.
    fn query(&self) -> Option<&CompiledQuery> {
        self.compiled
            .get_or_init(|| {
                CompiledQuery::compile(self.language.as_ref(), &self.source)
                    .ok()
                    // Only sound if tree-sitter numbered the patterns the way the
                    // concatenation did. It always has; checking turns a silent
                    // misattribution — one rule's matches handed to another — into a
                    // fallback.
                    .filter(|query| query.pattern_count() == self.owners.len())
            })
            .as_ref()
    }
}

/// Build one multi-pattern query per language, over every rule that declares it.
///
/// Rules are visited in `rules` order and their patterns appended in that order, so
/// `owners` is built alongside the source it describes and the two cannot drift.
///
/// Nothing is compiled here — see [`CombinedQuery::query`], which does it on first use so a
/// warm run never pays for a query it will not run.
fn combine_queries(rules: &[Prepared]) -> BTreeMap<String, CombinedQuery> {
    let mut sources: BTreeMap<String, Concatenation> = BTreeMap::new();

    for (index, rule) in rules.iter().enumerate() {
        for (language, query) in &rule.compiled {
            let entry = sources
                .entry(language.id().as_str().to_owned())
                .or_insert_with(|| Concatenation {
                    language: Arc::clone(language),
                    source: String::new(),
                    owners: Vec::new(),
                });
            entry.source.push_str(&rule.spec.query);
            // A query source need not end in a newline, and two patterns run together on
            // one line is a different query from the two of them.
            entry.source.push('\n');
            entry
                .owners
                .extend(std::iter::repeat_n(index, query.pattern_count()));
        }
    }

    sources
        .into_iter()
        .map(|(id, parts)| {
            (
                id,
                CombinedQuery {
                    language: parts.language,
                    source: parts.source,
                    owners: parts.owners,
                    compiled: std::sync::OnceLock::new(),
                },
            )
        })
        .collect()
}

/// Everything a run needs, built once and shared across workers.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent run modes — caching, reducing, unused reporting, profiling — \
              every combination of which is meaningful and reachable from the CLI. The lint \
              is aimed at a type where a pile of bools stands in for a missing enum; these \
              are orthogonal switches, and an enum over their sixteen combinations would be \
              strictly worse to read and to set."
)]
pub struct Engine {
    rules: Vec<Prepared>,
    /// One multi-pattern query per language, over every rule that declares it.
    ///
    /// Empty for a language no rule targets, and not built at all until a file needs it.
    /// Assembling the sources measured ~4 ms over the §15 ruleset, which a warm run — every
    /// file a cache hit, no query run — would have paid for nothing. Warm has the tightest
    /// budget of the three scenarios, so it does not subsidize cold.
    combined: std::sync::OnceLock<BTreeMap<String, CombinedQuery>>,
    discovery: Discovery,
    /// The project root, canonicalized once. Every tracked read is checked against it, and
    /// canonicalizing per file would put a syscall on the hot path for a constant.
    root: PathBuf,
    /// Everything constant about this run that a cache key depends on.
    run_key: RunKey,
    /// The component engine and the run's linked rule set, or `None` when no rule is backed
    /// by a component.
    ///
    /// `None` is the state every run in this tree is in today, and it is not merely an empty
    /// set: building one starts an epoch ticker thread and compiles nothing, so a run with no
    /// component rule must not build one at all.
    components: Option<Components>,
    /// Whether results may be read from and written to the cache.
    ///
    /// **Off for any run that loaded a component, and that is a correctness guard rather than a
    /// policy.** A component's bytes reach no cache-key input: `RuleSpec::component` is set on a
    /// `Config` *after* `lanekeep_config::load` computed `ruleset_hash`, and `load_components`
    /// reads the file with an untracked `std::fs::read`. So without this, swapping a rule's
    /// component for a different one between two runs serves the first one's answer forever —
    /// demonstrated by `swapping_a_component_between_runs_changes_the_answer`, which fails
    /// without it.
    ///
    /// **Refusing the cache rather than folding the bytes here**, for three reasons. The correct
    /// fold already exists in `lanekeep-config`'s `hash_ruleset` — sorted, deduplicated,
    /// length-prefixed, with a present/absent marker — and a second implementation of a
    /// cache-key encoding in a second crate is exactly the drift that produced this
    /// sub-project's one real cache bug, where reusing a text separator for arbitrary binary let
    /// two rulesets share a key. The fold belongs beside the existing one, on the day
    /// `lanekeep-config` can produce a component-backed rule at all — which it cannot, because
    /// the world has no `metadata` export, so this costs nothing today. And a guard that turns
    /// the cache *off* has no encoding to get wrong: the failure mode of getting it wrong is a
    /// cold run, where the failure mode of a wrong fold is a wrong answer.
    ///
    /// It is per run rather than per rule because a cache entry is per *file* and holds every
    /// rule's findings for it, so there is no finer granularity that is sound.
    caching: bool,
    /// Whether reduce phases run.
    reducing: bool,
    /// Whether directives that silenced nothing are reported.
    reporting_unused: bool,
    /// Whether per-rule timings are collected.
    profiling: bool,
    /// The date `expires:` is compared against.
    ///
    /// Fixed once for the run, so two files checked a millisecond apart cannot disagree
    /// about what day it is. Supplied by the host because the sandbox has no clock.
    today: Date,
    limits: Limits,
    rules_root: RuleRoot,
    config_path: PathBuf,
    typescript: Arc<dyn Language>,
    javascript: Arc<dyn Language>,
    /// Extension to language id, so a file can be matched to a grammar without the registry.
    ///
    /// Lowercased keys, because the registry lowercases too — whether `Button.TSX` gets
    /// checked should not depend on how someone typed it.
    languages_by_extension: BTreeMap<String, String>,
}

/// The component half of a run, walled off so its one constructor cannot be gone around.
///
/// **A module for two fields, and the module is the point.** `EXTERNAL_BINDINGS` enforcement
/// used to be a statement in `load_components`; deleting it left every test passing, because
/// nothing asserted the comparison was *invoked*. Moving it into a constructor fixed that and
/// left a second way to be wrong — a struct literal beside the constructor, which a mutation
/// confirmed still compiled and still passed. Private fields behind a module seam remove that
/// too, on exactly the reasoning `lanekeep_wasm::load::Loaded` uses for the import check: the
/// door that skips the check is the one that does not exist.
mod components {
    use std::sync::Arc;

    use lanekeep_wasm::{RuleSet, WasmEngine};

    use super::{RunError, declared_bindings_match};

    /// The component engine and the run's linked rule set.
    ///
    /// Two `Arc`s and nothing else, which is the arrangement `lanekeep-wasm` requires rather
    /// than a convenience: one [`WasmEngine`] because it is the unit compiled code is cached in
    /// and the owner of the one epoch ticker, and one [`RuleSet`] because `instantiate_pre`
    /// resolves and type-checks a component's imports independently of how many stores will
    /// instantiate it. Nothing here is instantiated — an instance belongs to a store, and a
    /// store belongs to a worker.
    pub(super) struct Components {
        engine: Arc<WasmEngine>,
        rules: Arc<RuleSet>,
    }

    impl Components {
        /// The only way to make one, and it is the only way because of what it checks.
        ///
        /// # Errors
        ///
        /// Returns [`RunError::Worker`] when the set bound an interface beside the declared
        /// world that `lanekeep_wasm::EXTERNAL_BINDINGS` — the list the cache key was computed
        /// from — does not name.
        pub(super) fn linked(engine: Arc<WasmEngine>, rules: RuleSet) -> Result<Self, RunError> {
            declared_bindings_match(rules.external_bindings())?;
            Ok(Self {
                engine,
                rules: Arc::new(rules),
            })
        }

        /// The shared engine, for building a worker's store.
        pub(super) const fn engine(&self) -> &Arc<WasmEngine> {
            &self.engine
        }

        /// The run's linked rule set.
        pub(super) const fn rules(&self) -> &Arc<RuleSet> {
            &self.rules
        }
    }
}

use components::Components;

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("rules", &self.rules.len())
            .field("root", &self.discovery.root())
            .finish_non_exhaustive()
    }
}

/// Where a run spent its time, per rule.
///
/// The split is the point. A rule that is slow in `query` has a query matching more than it
/// needs and wants narrowing; a rule that is slow in `handler` has code to look at. Reporting
/// one total would leave an author guessing which, and the two have nothing in common as
/// fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuleTiming {
    /// Time matching this rule's query, in Rust.
    pub query: Duration,
    /// Time inside its handler, in the sandbox.
    pub handler: Duration,
    /// How many matches crossed the boundary.
    ///
    /// The number the query gate exists to keep small — §7.2 — so it belongs beside the
    /// times rather than being inferred from them.
    pub matches: u64,
}

impl RuleTiming {
    /// Everything this rule cost.
    #[must_use]
    pub const fn total(&self) -> Duration {
        self.query.saturating_add(self.handler)
    }
}

/// What a run produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Violations, in canonical order.
    pub violations: Vec<Violation>,
    /// How many files discovery selected.
    pub files_discovered: usize,
    /// How many were actually parsed, after gates.
    pub files_parsed: usize,

    /// Where the run spent its time, per rule, when `--profile` asked.
    ///
    /// Absent otherwise: timing every match costs a clock read per invocation, which is
    /// exactly the kind of thing that should not be on the path a warm run takes.
    pub timings: Option<BTreeMap<RuleId, RuleTiming>>,

    /// What each checked file's rules read beyond that file, in path order.
    ///
    /// Exactly the shape a cache entry needs: dependencies belong to the file whose result
    /// they affect, not to the run. A file with no tracked reads has no entry here rather
    /// than an empty one, so the common case costs nothing.
    pub dependencies: BTreeMap<FilePath, Vec<TrackedRead>>,
}

impl Engine {
    /// Prepare a run.
    ///
    /// Everything that can fail on a rule's own contents fails here, before any file is
    /// read — a run that dies halfway through because rule seventeen has a typo has
    /// already wasted the work.
    ///
    /// # Errors
    ///
    /// Returns [`RunError`] for an invalid query, gate, or language reference.
    pub fn prepare(
        config: &Config,
        project_root: &Path,
        rules_root: RuleRoot,
        config_path: &Path,
        registry: &LanguageRegistry,
        typescript: Arc<dyn Language>,
        javascript: Arc<dyn Language>,
    ) -> Result<Self, RunError> {
        let discovery = Discovery::new(project_root, &config.include, &config.exclude)?;

        let known = registry
            .languages()
            .map(|l| l.id().as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let mut languages_by_extension = BTreeMap::new();
        for language in registry.languages() {
            for extension in language.extensions() {
                languages_by_extension
                    .insert(extension.to_ascii_lowercase(), language.id().to_string());
            }
        }

        // Compiled in parallel, because this is the single most expensive thing a run does
        // before it has looked at a file: a tree-sitter query costs a couple of milliseconds
        // to compile, and a rule compiles one per language it declares. Twenty rules over two
        // languages is forty compilations and most of a warm run's wall clock — measured at
        // ~88 ms against a ~55 ms warm run, so construction cost more than the work.
        //
        // Every compilation is independent, so this is parallelism with no shared state and
        // no ordering to preserve *during* it. What must stay ordered is the result: `rules`
        // is indexed by the config's rule order, and a run's violations are sorted by rule id,
        // so a shuffled `rules` would be a different program. `collect` into a `Vec<Result<_>>`
        // preserves input order regardless of completion order, which is what makes this safe.
        //
        // Deliberately *not* made lazy. Compiling on first use would take a warm run's cost to
        // nearly zero, and would cost the guarantee the comment below describes: a broken query
        // is reported here, naming its rule, rather than staying silent until some file happens
        // to need it.
        let prepared: Vec<Result<Prepared, RunError>> =
            config
                .rules
                .par_iter()
                .filter(|spec| spec.severity.is_enabled())
                .map(|spec| {
                    let mut compiled = Vec::with_capacity(spec.languages.len());
                    for id in &spec.languages {
                        let language = registry.by_id(id).cloned().ok_or_else(|| {
                            RunError::UnknownLanguage {
                                rule: spec.id.to_string(),
                                language: id.clone(),
                                known: known.clone(),
                            }
                        })?;

                        // Compiled against this grammar specifically. A query that is valid for
                        // one dialect and not another is a rule bug, and this is where it
                        // surfaces — at config load, naming the rule, rather than as silence at
                        // run time.
                        let query = CompiledQuery::compile(language.as_ref(), &spec.query)
                            .map_err(|e: CompileError| RunError::Query {
                                rule: spec.id.to_string(),
                                detail: e.to_string(),
                            })?;

                        compiled.push((language, query));
                    }

                    let gates =
                        CompiledGates::compile(&spec.gates).map_err(|e| RunError::Gates {
                            rule: spec.id.to_string(),
                            detail: e.to_string(),
                        })?;

                    Ok(Prepared {
                        // Filled in below, once config order is known.
                        index: 0,
                        spec: spec.clone(),
                        gates,
                        compiled,
                        // Filled in below too, by the one place components are loaded.
                        slot: None,
                    })
                })
                .collect();

        // The first failure by *config order*, not by whichever thread finished first. Two
        // broken rules must always name the same one, or the same project reports a different
        // error between runs.
        let mut rules = Vec::with_capacity(prepared.len());
        for result in prepared {
            rules.push(result?);
        }
        for (index, rule) in rules.iter_mut().enumerate() {
            rule.index = index;
        }

        // Every component this run will execute, compiled, import-checked and linked against
        // the host world — once, here, before any worker exists. A rule whose bytes are
        // missing or whose imports reach past the sandbox fails now, naming itself, rather
        // than on whichever file happened to match it first.
        let components = load_components(&mut rules, project_root)?;

        // Every registered grammar, so a tree-sitter bump invalidates rather than silently
        // reusing results computed against different node shapes.
        let mut grammars: Vec<GrammarKey> = registry
            .languages()
            .map(|language| GrammarKey {
                id: language.id().to_string(),
                abi: u32::try_from(language.grammar_abi()).unwrap_or(u32::MAX),
            })
            .collect();
        grammars.sort_by(|a, b| a.id.cmp(&b.id));

        let run_key = run_key(&config.ruleset_hash, &config.config_hash, &grammars)?;

        Ok(Self {
            rules,
            combined: std::sync::OnceLock::new(),
            run_key,
            // Off whenever a component was loaded — see the field. Written from `components`
            // rather than from the rule list so the two cannot disagree about whether this run
            // has a component in it.
            caching: components.is_none(),
            components,
            reducing: true,
            reporting_unused: false,
            profiling: false,
            today: suppression::today(),
            // Canonicalized here so every tracked read compares against the same absolute
            // root. Falling back to the path as given keeps a non-existent root a discovery
            // problem rather than turning it into a confusing read failure later.
            root: project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf()),
            discovery,
            limits: config.limits,
            rules_root,
            config_path: config_path.to_path_buf(),
            typescript,
            javascript,
            languages_by_extension,
        })
    }

    /// Which language parses this file, or `None` when nothing registered claims it.
    fn language_of(&self, path: &FilePath) -> Option<&str> {
        let extension = Path::new(path.as_str())
            .extension()?
            .to_str()?
            .to_ascii_lowercase();
        self.languages_by_extension
            .get(extension.as_str())
            .map(String::as_str)
    }

    /// Turn the cache off, for `--no-cache` and for tests that need a cold run.
    #[must_use]
    pub const fn without_cache(mut self) -> Self {
        self.caching = false;
        self
    }

    /// Collect per-rule timings.
    ///
    /// Off by default because measuring costs a clock read per handler invocation, and the
    /// path a warm run takes is the one place that matters most.
    #[must_use]
    pub const fn profiling(mut self) -> Self {
        self.profiling = true;
        self
    }

    /// Report suppressions that silenced nothing.
    ///
    /// Off by default because it is hygiene rather than correctness: a suppression whose
    /// violation no longer exists is debt, and debt is worth surfacing on request rather
    /// than in everyone's inner loop.
    #[must_use]
    pub const fn reporting_unused_suppressions(mut self) -> Self {
        self.reporting_unused = true;
        self
    }

    /// Fix the date `expires:` is compared against.
    ///
    /// For tests, which otherwise could not assert anything about expiry without waiting.
    #[must_use]
    pub const fn with_today(mut self, today: Date) -> Self {
        self.today = today;
        self
    }

    /// Skip every reduce phase.
    ///
    /// For a run over a deliberately partial corpus. A cross-file rule consumes facts from
    /// every file, so running one over a subset does not give a smaller answer — it gives a
    /// wrong one. `no-unused-exports` over three changed files would report every export in
    /// them as unused, because the importers were never looked at.
    ///
    /// Skipping is therefore the only sound option, and the caller that narrowed the corpus
    /// is the one that has to say so to the user.
    #[must_use]
    pub const fn without_reduce(mut self) -> Self {
        self.reducing = false;
        self
    }

    /// The files discovery selects, before any gate.
    ///
    /// For a caller narrowing the corpus: intersecting with this is what keeps `include` and
    /// `exclude` in force, so `--staged` cannot check a file the config excluded.
    #[must_use]
    pub fn discover(&self) -> Vec<FilePath> {
        self.discovery.walk()
    }

    /// How many rules will actually run. Rules set to `off` are dropped at preparation.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// The rules that will run, in the order the config declared them.
    ///
    /// The specs rather than a rendered listing: what a listing should look like is the
    /// reporter's problem, and an engine that decided it would have to be changed for every
    /// new output format.
    pub fn rules(&self) -> impl Iterator<Item = &RuleSpec> {
        self.rules.iter().map(|prepared| &prepared.spec)
    }

    /// Run over the whole corpus.
    ///
    /// # Errors
    ///
    /// Returns the first [`RunError`] any worker produced. Rayon's reduction is not
    /// order-dependent, so which of several simultaneous failures surfaces is arbitrary —
    /// but every one of them aborts the run, so the choice does not change the outcome.
    pub fn run(&self) -> Result<Outcome, RunError> {
        let files = self.discovery.walk();
        self.run_files(&files, Coverage::Whole)
    }

    /// Run over an explicit file list, for `--since` and `--staged`.
    ///
    /// # Errors
    ///
    /// As [`Engine::run`].
    pub fn run_over(&self, files: &[FilePath]) -> Result<Outcome, RunError> {
        self.run_files(files, Coverage::Partial)
    }

    /// The shared body of [`Engine::run`] and [`Engine::run_over`].
    fn run_files(&self, files: &[FilePath], coverage: Coverage) -> Result<Outcome, RunError> {
        let clock = RunClock::start(self.limits.global_timeout);

        // Loaded once, before any worker starts. Shared read-only across the pool: a cache
        // that workers wrote to concurrently would need a lock on the hot path, and the
        // whole point is to be faster than recomputing.
        let cache = if self.caching {
            Store::load(&self.root)
        } else {
            Store::empty()
        };

        let results: Vec<Result<FileOutcome, RunError>> = files
            .par_iter()
            .map_init(
                // One sandbox per worker, created on first use and reused for that
                // worker's whole share. Building one per file would pay engine startup
                // thousands of times; sharing one across workers is impossible, since the
                // runtime is single-threaded by construction.
                // The sandbox is per worker and built on first use — one engine startup
                // per thread that needs one, rather than per file, and none at all for a
                // worker whose files all hit the cache. That last part is what makes a warm
                // run cheap: starting QuickJS and evaluating every rule module, per worker,
                // to then execute no JavaScript, was most of a warm run's cost.
                || Worker::new(self, &clock),
                |worker, path| self.check_file(worker, &cache, path),
            )
            .collect();

        let mut violations = Vec::new();
        let mut facts = Vec::new();
        let mut files_parsed = 0;
        let mut dependencies = BTreeMap::new();
        let mut fresh = Store::empty();
        let mut directives: BTreeMap<FilePath, FileDirectives> = BTreeMap::new();
        let mut timings: BTreeMap<RuleId, RuleTiming> = BTreeMap::new();
        // The first failure by *file order*, kept rather than returned, because the entries
        // every other file produced are still owed to the cache — see the save below. Which
        // failure is reported does not change: it is the same one `?` would have taken, since
        // rayon's `collect` preserves input order.
        let mut failure: Option<RunError> = None;
        for result in results {
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    failure.get_or_insert(error);
                    continue;
                }
            };
            violations.extend(outcome.violations);
            facts.extend(outcome.facts);
            files_parsed += usize::from(outcome.parsed);
            if let Some(entry) = outcome.entry {
                fresh.insert(entry.0, entry.1);
            }
            for (rule, timing) in outcome.timings {
                let entry = timings.entry(rule).or_default();
                entry.query = entry.query.saturating_add(timing.query);
                entry.handler = entry.handler.saturating_add(timing.handler);
                entry.matches += timing.matches;
            }
            if !outcome.suppressions.is_empty() {
                directives.insert(
                    outcome.path.clone(),
                    FileDirectives {
                        suppressions: outcome.suppressions,
                        used: outcome.used_suppressions,
                    },
                );
            }
            if !outcome.reads.is_empty() {
                dependencies.insert(outcome.path, outcome.reads);
            }
        }

        // Saved before the failure is propagated, and merged rather than pruned when there is
        // one.
        //
        // §6.8: a limit breach cancels the run, and **cache entries for files that fully
        // completed are still committed**. Each is independently valid — it records every rule
        // running against those bytes to completion — and dropping them means a corpus that
        // dies on a cold run dies identically on every retry, with no way to make progress.
        // That was latent while nothing enforced the run budget outside a handler, because a
        // corpus of cheap invocations simply finished; the check at the top of `check_file` is
        // what turns it into the ordinary case.
        //
        // Pruning is the part that must not happen. A run that stopped early holds entries for
        // a fraction of the corpus and never looked at the rest, so a fresh-only save would age
        // out every file it never reached and leave the next run *colder* than the one that
        // failed. That is the same reasoning `Coverage::Partial` already carries, arrived at
        // from the other direction: pruning is sound only for a run that saw everything, and an
        // aborted run did not.
        if self.caching {
            match coverage {
                // The run saw everything, so what it did not produce an entry for no longer
                // exists. Saving only fresh entries is what ages deleted files out.
                Coverage::Whole if failure.is_none() => fresh.save(&self.root),
                // The run saw a subset — because it was given one, or because it stopped part
                // way through. Saving only what it produced would discard the entries for
                // every file it never looked at, so `--staged` would leave the next full run
                // cold, which is the opposite of what an incremental entry point is for.
                Coverage::Whole | Coverage::Partial => {
                    let mut merged = cache;
                    for key in fresh.keys().copied().collect::<Vec<_>>() {
                        if let Some(entry) = fresh.get(&key) {
                            merged.insert(key, entry.clone());
                        }
                    }
                    merged.save(&self.root);
                }
            }
        }

        if let Some(error) = failure {
            return Err(error);
        }

        // Into the one order every run will see, before any rule looks at them.
        //
        // Rayon's `collect` into a `Vec` already preserves input order, so on today's code
        // path this sort changes nothing — which is exactly why it is easy to delete and
        // must not be. The ordering guarantee belongs to the engine, not to a property of
        // whichever collection strategy it happens to use: switching to `for_each` with a
        // shared sink, or grouping by rule before reducing, would silently lose it. The
        // cost is one sort of a small vector, once per run.
        lanekeep_core::fact::sort(&mut facts);

        // A cross-file rule reports at a site in some other file, which may well have been a
        // cache hit this run — so its directives come from the outcome, whether they were
        // parsed now or restored from the entry.
        let reduced = self.reduce(&clock, files, &facts)?;
        for violation in reduced {
            // A cross-file violation can be the only thing a directive ever silences, so
            // usage is recorded here too — otherwise it would be reported as unused.
            match covering_elsewhere(&directives, &violation) {
                Some((file, index)) => {
                    if let Some(found) = directives.get_mut(&file)
                        && !found.used.contains(&index)
                    {
                        found.used.push(index);
                    }
                }
                None => violations.push(violation),
            }
        }

        if self.reporting_unused {
            violations.extend(unused_violations(&directives));
        }

        lanekeep_core::sort(&mut violations);
        Ok(Outcome {
            violations,
            files_discovered: files.len(),
            files_parsed,
            timings: self.profiling.then_some(timings),
            dependencies,
        })
    }

    /// Run the reduce phase for every rule that has one.
    ///
    /// Single-threaded, and deliberately so: there is one pass per rule, each already sees
    /// the whole corpus, and a rule's `reduce` is the one place a rule is allowed to be
    /// expensive. Parallelizing across rules would buy little and would need one sandbox per
    /// worker with the whole fact set copied into each.
    fn reduce(
        &self,
        clock: &Arc<RunClock>,
        files: &[FilePath],
        facts: &[Fact],
    ) -> Result<Vec<Violation>, RunError> {
        if !self.reducing {
            return Ok(Vec::new());
        }

        let reducing: Vec<&Prepared> = self
            .rules
            .iter()
            .filter(|rule| rule.spec.has_reduce)
            .collect();
        if reducing.is_empty() {
            // The common case. Building a sandbox to do nothing would put engine startup on
            // the critical path of every run that has no cross-file rule at all.
            return Ok(Vec::new());
        }

        let paths: Vec<String> = files.iter().map(|f| f.as_str().to_owned()).collect();
        let mut violations = Vec::new();

        // Each engine's cross-file pass, and each built only if something needs it. A ruleset
        // whose only cross-file rule is a component must not start QuickJS and evaluate every
        // module into it, and the reverse holds just as strongly: building a component runtime
        // spawns the epoch ticker.
        let (module_rules, component_rules): (Vec<&Prepared>, Vec<&Prepared>) =
            reducing.into_iter().partition(|rule| rule.slot.is_none());

        if !component_rules.is_empty() {
            violations.extend(self.reduce_components(clock, &component_rules, &paths, facts)?);
        }
        if module_rules.is_empty() {
            return Ok(violations);
        }

        let sandbox = self.build_sandbox(clock)?;

        for rule in module_rules {
            // A rule sees only its own facts. Letting one read another's would make an
            // internal payload shape into a contract between rules, and would make the
            // result depend on the order rules happened to be declared in.
            let own: Vec<ReduceFact> = facts
                .iter()
                .filter(|fact| fact.rule_id == rule.spec.id)
                .map(|fact| ReduceFact {
                    kind: fact.kind.clone(),
                    json: lanekeep_js::merge_file(&fact.data, fact.file.as_str()),
                })
                .collect();

            let host = ReduceContext::new(paths.clone(), own);
            let timeout = rule.spec.timeout.unwrap_or(self.limits.rule_timeout);
            let call = format!(
                "globalThis.__lanekeepConfig.rules[{}].reduce(ctx)",
                rule_index(&rule.spec)
            );

            sandbox
                .eval_with_reduce_host::<()>(&host, &call, timeout)
                .map_err(|e: SandboxError| RunError::Rule {
                    rule: rule.spec.id.to_string(),
                    // No single file is at fault in a reduce phase, and naming one would be
                    // a lie the reader would then go and look at.
                    file: "<reduce>".to_owned(),
                    detail: e.to_string(),
                })?;

            for report in host.take_reports() {
                // The path is the rule's, normalized but not checked against the corpus. A
                // cross-file rule may legitimately report at a file the walker excluded —
                // a config, a generated file. Autofix will need to disagree: it must never
                // write to a path a rule invented. That check belongs with the writing.
                violations.push(Violation {
                    rule_id: rule.spec.id.clone(),
                    location: Location::new(
                        FilePath::new(&report.file),
                        Position::new(report.line, report.column),
                    ),
                    message: report
                        .message
                        .unwrap_or_else(|| rule.spec.card.message.clone()),
                    remediation: rule.spec.card.remediation.clone(),
                    severity: rule.spec.severity,
                    // A reduce phase has no parse tree, so there is no node to replace and
                    // nothing to compute a byte range from. A cross-file finding is fixed by
                    // hand.
                    fix: None,
                });
            }
        }

        Ok(violations)
    }

    /// The cross-file pass for every component-backed rule that has one.
    ///
    /// One store for the whole phase, instantiating each reducing rule once. It is not a
    /// worker's store: workers are gone by now, and a reduce pass is single-threaded.
    ///
    /// # A fact's file is a field, and this is where getting that wrong would have shown
    ///
    /// The JavaScript path splices `"file"` into the payload with `lanekeep_js::merge_file`,
    /// because its `ReduceFact` carries only `kind` and `json` and a rule reads `fact.file` off
    /// the parsed object. The world's `emitted-fact` has a `file` field of its own, so the
    /// component path carries it there and **must not** merge. Doing both produces a payload
    /// with a literal duplicate `"file"` key — valid enough for most parsers to accept and
    /// silently pick one of, and invisible from the host side, because the host forwards `data`
    /// exactly as the guest wrote it.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Rule`] for a trapping guest or a breached budget, and
    /// [`RunError::Worker`] when the runtime cannot be built.
    fn reduce_components(
        &self,
        clock: &Arc<RunClock>,
        reducing: &[&Prepared],
        paths: &[String],
        facts: &[Fact],
    ) -> Result<Vec<Violation>, RunError> {
        let components = self.components.as_ref().ok_or_else(|| RunError::Worker {
            detail: "a component rule has a reduce phase in a run that loaded no components"
                .to_owned(),
        })?;
        let mut runtime = WasmRuntime::for_rules(
            Arc::clone(components.engine()),
            Arc::clone(components.rules()),
            self.limits,
            Arc::clone(clock),
        );

        let mut violations = Vec::new();
        for rule in reducing {
            let Some(slot) = rule.slot else { continue };
            let fail = |detail: String| RunError::Rule {
                rule: rule.spec.id.to_string(),
                // No single file is at fault in a reduce phase, and naming one would be a lie
                // the reader would then go and look at.
                file: "<reduce>".to_owned(),
                detail,
            };

            // A rule sees only its own facts, exactly as on the JavaScript path, and in the
            // order `lanekeep_core::fact::sort` already put them in.
            let own: Vec<types::EmittedFact> = facts
                .iter()
                .filter(|fact| fact.rule_id == rule.spec.id)
                .map(|fact| types::EmittedFact {
                    kind: fact.kind.clone(),
                    file: fact.file.as_str().to_owned(),
                    data: fact.data.clone(),
                })
                .collect();

            let resource = runtime
                .host_mut()
                .push_reduce_context(ComponentReduceContext::new(paths.to_vec(), own))
                .map_err(|e| fail(e.to_string()))?;

            let timeout = rule.spec.timeout.unwrap_or(self.limits.rule_timeout);
            let outcome = runtime.reduce_with_timeout(slot, &resource, timeout);

            // Taken before the failure is propagated, so the context does not outlive the call
            // that needed it even on the path that ends the run.
            let mut taken = runtime
                .host_mut()
                .take_reduce_context(resource)
                .map_err(|e| fail(e.to_string()))?;
            outcome.map_err(|e: WasmError| fail(e.to_string()))?;

            for report in taken.take_reports() {
                // The path is the rule's, normalized but not checked against the corpus — the
                // same posture the JavaScript path takes, for the same reason.
                violations.push(Violation {
                    rule_id: rule.spec.id.clone(),
                    location: Location::new(
                        FilePath::new(&report.file),
                        Position::new(report.line, report.column),
                    ),
                    message: report
                        .message
                        .unwrap_or_else(|| rule.spec.card.message.clone()),
                    remediation: rule.spec.card.remediation.clone(),
                    severity: rule.spec.severity,
                    // A reduce phase has no parse tree, so there is no node to replace.
                    fix: None,
                });
            }
        }

        Ok(violations)
    }

    /// Build the sandbox a worker uses, evaluating the ruleset into it.
    fn build_sandbox(&self, clock: &Arc<RunClock>) -> Result<Sandbox, RunError> {
        let sandbox = Sandbox::with_modules(
            self.limits,
            Arc::clone(clock),
            self.rules_root.clone(),
            Arc::clone(&self.typescript),
            Arc::clone(&self.javascript),
        )
        .map_err(|e| RunError::Worker {
            detail: e.to_string(),
        })?;

        // Every worker evaluates the ruleset into its own engine. A rule's `check` is a
        // function, and a function cannot cross between runtimes — so the modules are
        // loaded per worker rather than the handlers being extracted and shared.
        lanekeep_config::evaluate_into(&sandbox, &self.rules_root, &self.config_path).map_err(
            |e: ConfigError| RunError::Worker {
                detail: e.to_string(),
            },
        )?;

        Ok(sandbox)
    }

    /// Check one file. Returns its violations, facts and tracked reads.
    fn check_file(
        &self,
        worker: &mut Worker<'_>,
        cache: &Store,
        path: &FilePath,
    ) -> Result<FileOutcome, RunError> {
        // **The run's budget, asked where the run's time is actually spent.**
        //
        // Both engines poll it from inside a handler and nowhere else: QuickJS from its
        // interrupt handler, wasmtime from the epoch checks Cranelift compiles into guest
        // code. Neither runs while this engine is reading a file, hashing it, parsing it or
        // evaluating a query — and §15 says that is most of a cold run. So a rule whose
        // handler returns after a handful of operations could overrun the budget without ever
        // being asked to stop: `AGENTS.md` recorded four hundred files against a one-line rule
        // running to completion under a one-millisecond budget, and the component path had the
        // same gap for the same reason.
        //
        // One check, here, closes it for both, because this sits above the dispatch that
        // chooses between them. A file boundary is also the only place a run *can* be stopped
        // without degrading it: everything before this line for this file has not happened
        // yet, and everything after it happens in full or not at all.
        //
        // It costs one clock read per file, on a path that already reads the file from disk.
        // That matters because `Worker`'s own count is per rayon *chunk* rather than per
        // thread — but this is per file either way, and `RunClock::is_expired` allocates
        // nothing and takes no lock.
        if worker.clock.is_expired() {
            return Err(RunError::RunTimeout {
                budget: worker.clock.global_timeout(),
                elapsed: worker.clock.elapsed(),
            });
        }

        // A fresh set of tracked reads for this file, sharing the root already canonicalized
        // at preparation.
        //
        // **One per file, and now shared by both engines rather than one per engine.** An
        // `Arc` rather than an `Rc` because `lanekeep_wasm::host::CheckContext` has to be
        // `Send`; the sharing itself is the point, since two memos over one file would let two
        // rules see a file rewritten between them differently, and the two dependency lists
        // could not be merged afterwards — `tracked::sort` orders by path and does not dedupe,
        // so a disagreement about one path becomes two contradictory entries for it.
        let files = Arc::new(FileAccess::rooted(self.root.clone()));

        // Path gates first: rejecting here costs no read at all.
        //
        let admitted: Vec<&Prepared> = self
            .rules
            .iter()
            .filter(|rule| rule.gates.admits_path(path))
            .collect();
        if admitted.is_empty() {
            return Ok(FileOutcome::skipped(path.clone()));
        }

        let absolute = self.discovery.root().join(path.as_str());
        let Ok(bytes) = std::fs::read(&absolute) else {
            // A file that vanished between discovery and reading is not a failure. The
            // tree is allowed to change under a run; what must not happen is a partial
            // result being reported as complete, and a missing file contributes nothing
            // either way.
            return Ok(FileOutcome::skipped(path.clone()));
        };

        // The cache is consulted after the path gates and the read, because the key needs
        // the file's bytes — but before the content gates and the parse, which is where the
        // saving is. A hit costs one hash and one dependency check.
        // A file's result can depend on what day it is, two ways: an expiring suppression in
        // its bytes, or a rule that read `ctx.today` while checking it. Such a file gets a
        // key with the date folded in, so its entry lives for one day; every other file gets
        // a dateless key and its entry survives indefinitely.
        //
        // Folding the date into every key instead would invalidate the whole corpus daily
        // for the sake of a handful of files. Leaving it out entirely would serve yesterday's
        // answer — an expiry that never expires, a date comparison frozen at whenever the
        // cache was written.
        //
        // The expiry is visible in the bytes, so it is known now. Whether a rule reads the
        // date is not knowable until the rules have run, which is why both keys exist and
        // the lookup tries the dated one first: a file that was date-dependent last run has
        // its entry there, and if the date has moved that key simply misses.
        let keys = self.caching.then(|| {
            let content = lanekeep_cache::hash_bytes(&bytes);
            (
                self.run_key.for_file(path.as_str(), &content),
                self.run_key
                    .for_dated_file(path.as_str(), &content, &self.today.to_string()),
            )
        });
        let has_expiry = memchr::memmem::find(&bytes, b"expires:").is_some();

        if let Some((plain, dated)) = keys {
            // Dated first. A file with an expiring suppression is *only* ever stored dated,
            // so trying the plain key for it would be a lookup that can never hit.
            let candidates: &[CacheKey] = if has_expiry {
                &[dated]
            } else {
                &[dated, plain]
            };
            for key in candidates {
                if let Some(entry) = cache.get(key)
                    && lanekeep_cache::validate(entry, &self.root)
                {
                    return Ok(FileOutcome::cached(path.clone(), *key, entry.clone()));
                }
            }
        }

        // Content gates: one read, a substring scan, and a parse saved.
        let admitted: Vec<&Prepared> = admitted
            .into_iter()
            .filter(|rule| rule.gates.admits_content(&bytes))
            .collect();
        if admitted.is_empty() {
            // Still worth an entry: "nothing applies to this file" is a result, and
            // recomputing the gates every run for a file that never matches is the cost the
            // cache exists to remove. No rule ran, so nothing read the date — unless the
            // file carries an expiry, which is a property of its bytes.
            return Ok(FileOutcome::empty_entry(
                path.clone(),
                keys.map(|(plain, dated)| if has_expiry { dated } else { plain }),
            ));
        }

        let Ok(source) = String::from_utf8(bytes) else {
            // Not valid UTF-8, so not source this tool can reason about.
            return Ok(FileOutcome::skipped(path.clone()));
        };

        // Parsed once per file, whatever rules ran: a directive is a property of the file,
        // not of any rule.
        let directives = suppression::parse(&source);

        let mut outcome = FileOutcome::parsed(path.clone());
        let Some((language, tree)) = self.parse_once(path, &source, &admitted) else {
            return Ok(outcome);
        };

        // One traversal for every rule, where the ruleset allows it. `collected[i]` holds
        // the matches for `self.rules[i]`; `None` means no combined pass ran and each rule
        // walks the tree itself.
        let file = FileUnderCheck {
            path,
            source: &source,
            tree: &tree,
            language: &language,
        };
        self.dispatch(worker, &files, &admitted, &file, &mut outcome)?;
        self.apply_directives(&mut outcome, &directives, path);

        outcome.suppressions = directives.valid;
        outcome.reads = files.dependencies();
        // Dated if anything about this file's result depended on the date: an expiring
        // directive, or a rule that read `ctx.today`.
        let date_dependent = has_expiry || outcome.read_the_date;
        outcome.entry = keys.map(|(plain, dated)| {
            (
                if date_dependent { dated } else { plain },
                CacheEntry {
                    violations: outcome.violations.clone(),
                    facts: outcome.facts.clone(),
                    dependencies: outcome.reads.clone(),
                    suppressions: outcome.suppressions.clone(),
                    used_suppressions: outcome.used_suppressions.clone(),
                },
            )
        });

        Ok(outcome)
    }

    /// Run every admitted rule over one parsed file, through whichever engine backs it.
    ///
    /// **The dispatch, and it is one `if let` on one field.** Both arms produce the same four
    /// things, so nothing downstream — sorting, suppression, the cache entry — can tell which
    /// engine an answer came from. That is the requirement rather than a nicety: two engines
    /// feeding one output must not introduce a second ordering or a second shape of result.
    fn dispatch(
        &self,
        worker: &mut Worker<'_>,
        files: &Arc<FileAccess>,
        admitted: &[&Prepared],
        file: &FileUnderCheck<'_>,
        outcome: &mut FileOutcome,
    ) -> Result<(), RunError> {
        let mut collected = self.collect_matches(file, admitted);

        // One component context for the whole file, opened by the first component rule that has
        // a match and shared by every one after it. Per file rather than per rule for the reason
        // `files` is: it is what makes the arena, the query cache and — through `files` — the
        // read memo one thing rather than one per rule.
        let mut context: Option<Resource<CheckContext>> = None;

        for rule in admitted {
            // Taken, not cloned: each bucket is read exactly once, and copying capture
            // paths per rule would give back a share of what the single traversal saved.
            let matches = collected.as_mut().map(|by_rule| {
                by_rule
                    .get_mut(rule.index)
                    .map(std::mem::take)
                    .unwrap_or_default()
            });

            let (violations, facts, read_the_date, timing) = if let Some(slot) = rule.slot {
                let mut pass = ComponentPass {
                    files,
                    context: &mut context,
                };
                let outcome = self.run_component_rule(worker, &mut pass, rule, slot, file, matches);
                worker.poison_on(&outcome)?
            } else {
                self.run_rule(worker, files, rule, file, matches)?
            };

            outcome.violations.extend(violations);
            outcome.facts.extend(facts);
            outcome.read_the_date |= read_the_date;
            if self.profiling {
                outcome.timings.push((rule.spec.id.clone(), timing));
            }
        }

        // Give the store its entry back. A context holds the parse tree and the file's whole
        // source, so leaving one behind per file would grow a worker's store with the corpus —
        // charged against the same per-store memory ceiling a rule is charged against, and
        // silent until a large enough run.
        if let Some(resource) = context.take() {
            let taken = worker
                .runtime()?
                .host_mut()
                .take_check_context(resource)
                .map_err(|e| RunError::Rule {
                    rule: "<components>".to_owned(),
                    file: file.path.as_str().to_owned(),
                    detail: e.to_string(),
                })?;
            // Read once for the file rather than per rule: the flag is sticky for the life of
            // the context, so any component rule that asked dates this file's cache entry.
            outcome.read_the_date |= taken.date_was_read();
        }

        Ok(())
    }

    /// Violations about the directives themselves.
    ///
    /// A suppression that does not work has to say so. A malformed directive silences
    /// nothing while looking like it does, and an expired one is a deadline the author set
    /// and then passed — reporting both is the whole reason the fields are checked rather
    /// than best-effort parsed.
    fn directive_violations(&self, directives: &Suppressions, path: &FilePath) -> Vec<Violation> {
        let mut violations = Vec::new();

        // Parsed once here rather than per violation. `SUPPRESSION_RULE` is a literal this
        // crate controls, so a failure would be a build-time mistake — falling back to the
        // rules' own namespace keeps that from being a panic in a checker.
        let Ok(rule_id) = SUPPRESSION_RULE.parse::<RuleId>() else {
            return violations;
        };

        for bad in &directives.malformed {
            violations.push(Violation {
                rule_id: rule_id.clone(),
                location: Location::new(path.clone(), Position::new(bad.line, bad.column)),
                message: bad.problem.clone(),
                remediation: String::from(
                    "fix the directive, or remove it and fix what it was hiding",
                ),
                severity: Severity::Error,
                fix: None,
            });
        }

        for suppression in &directives.valid {
            let Some(expires) = suppression.expires else {
                continue;
            };
            if expires >= self.today {
                continue;
            }

            violations.push(Violation {
                rule_id: rule_id.clone(),
                location: Location::new(
                    path.clone(),
                    Position::new(suppression.line, suppression.column),
                ),
                message: format!(
                    "suppression expired on {expires} — \"{}\"",
                    suppression.reason
                ),
                remediation: String::from(
                    "fix what it was suppressing, or decide it is permanent and drop the \
                     expiry",
                ),
                severity: Severity::Error,
                fix: None,
            });
        }

        violations
    }

    /// Parse a file once, for every rule that will run on it.
    ///
    /// §2's "run compiled queries (one pass)" and §7's "single shared parse". `run_rule` built
    /// its own parser instead, so a file admitted by twenty rules was parsed twenty times —
    /// most of a cold run, and invisible, because parsing per rule produces identical output.
    ///
    /// The grammar comes from the rules rather than from a registry the engine would have to
    /// hold: every admitted rule that targets this file targets the same grammar for it, so
    /// the first one that does is as good as any.
    ///
    /// `None` when the language is unknown or the grammar cannot parse the file. That is not
    /// an error — it is what the per-rule early returns did before, and callers depend on a
    /// file like that simply producing no violations.
    /// Run every admitted rule's patterns in one traversal, bucketed by rule.
    ///
    /// `None` means the caller should fall back to a query per rule: either no combined
    /// query exists for this language, or `--profile` is on. Profiling deliberately takes
    /// the slow path, because the per-rule split it reports — query time against handler
    /// time — is a measurement of one rule in isolation, and a shared traversal has no
    /// honest way to divide itself between the rules that share it. See §15.
    ///
    /// Patterns belonging to rules a gate excluded still run; their matches are dropped
    /// here rather than never produced. That costs a little evaluation and saves the
    /// traversal, and it cannot change a result: a gate that rejects a file means the rule
    /// does not run on it, and a bucket that is thrown away is a rule that did not run.
    fn collect_matches(
        &self,
        file: &FileUnderCheck<'_>,
        admitted: &[&Prepared],
    ) -> Option<MatchesByRule> {
        if self.profiling {
            return None;
        }
        let FileUnderCheck {
            path,
            source,
            tree,
            language: _,
        } = *file;
        let combined = self
            .combined
            .get_or_init(|| combine_queries(&self.rules))
            .get(self.language_of(path)?)?;

        // One arena for the whole file, only to turn nodes into paths. Each rule still gets
        // its own arena and its own handles; a path is structural, so it crosses freely.
        let arena = lanekeep_js::NodeArena::new(tree.clone(), source.to_owned());

        let mut by_rule: MatchesByRule = vec![Vec::new(); self.rules.len()];
        let wanted: Vec<bool> = {
            let mut wanted = vec![false; self.rules.len()];
            for rule in admitted {
                wanted[rule.index] = true;
            }
            wanted
        };

        combined
            .query()?
            .for_each_match(arena.tree(), source.as_bytes(), |m| {
                let Some(&owner) = combined.owners.get(m.pattern_index) else {
                    return;
                };
                if !wanted[owner] {
                    return;
                }
                let captures = m
                    .captures
                    .iter()
                    .filter_map(|(name, node)| {
                        arena.path_of(*node).map(|path| ((*name).to_owned(), path))
                    })
                    .collect();
                by_rule[owner].push(captures);
            });

        Some(by_rule)
    }

    /// Drop the violations this file's directives silence, and record which fired.
    ///
    /// Applied after every rule has run, so a directive covers whatever any of them
    /// reported at that line. Which directive fired is recorded rather than discarded: it
    /// is the only moment the information exists, since a warm run sees the survivors and
    /// not what was hidden.
    fn apply_directives(
        &self,
        outcome: &mut FileOutcome,
        directives: &Suppressions,
        path: &FilePath,
    ) {
        let mut used = Vec::new();
        outcome.violations.retain(|violation| {
            match directives.covering(&violation.rule_id, violation.location.position.line) {
                Some(index) => {
                    let index = u32::try_from(index).unwrap_or(u32::MAX);
                    if !used.contains(&index) {
                        used.push(index);
                    }
                    false
                }
                None => true,
            }
        });
        used.sort_unstable();
        outcome.used_suppressions = used;
        outcome
            .violations
            .extend(self.directive_violations(directives, path));
    }

    /// Parse the file, and hand back the grammar that did it alongside the tree.
    ///
    /// The grammar comes back because the component engine needs it once per file rather than
    /// once per rule — see [`FileUnderCheck::language`] — and because looking it up a second
    /// time would be a second answer to a question that already has one.
    fn parse_once(
        &self,
        path: &FilePath,
        source: &str,
        admitted: &[&Prepared],
    ) -> Option<(Arc<dyn Language>, tree_sitter::Tree)> {
        let language_id = self.language_of(path)?;
        let (language, _) = admitted
            .iter()
            .find_map(|rule| rule.for_language(language_id))?;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        let tree = parser.parse(source, None)?;
        Some((Arc::clone(language), tree))
    }

    /// Run one component-backed rule over one file.
    ///
    /// The counterpart of [`Engine::run_rule`], and deliberately the same signature and the
    /// same four return values: a caller must not be able to tell which engine answered.
    ///
    /// # What is *not* here, and that is the simplification
    ///
    /// No source text is manufactured and nothing is parsed on the hot path. The JavaScript
    /// path builds `globalThis.__lanekeepConfig.rules[i].check(ctx, {…})` per match and hands
    /// it to a parser; here the captures become a WIT `match` — a list of name/handle pairs —
    /// and the rule's typed `check` export is called with it.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Rule`] for a trapping guest or a breached budget, both of which
    /// cancel the run. That is what keeps a poisoned store from being reused: any host refusal
    /// traps, and `imports: { default: trappable }` marks the whole store unenterable with no
    /// way to reset it — so a store that has trapped must never see another file, and every
    /// error here is propagated rather than skipped.
    fn run_component_rule(
        &self,
        worker: &mut Worker<'_>,
        pass: &mut ComponentPass<'_>,
        rule: &Prepared,
        slot: RuleSlot,
        file: &FileUnderCheck<'_>,
        precollected: Option<RuleMatches>,
    ) -> Result<(Vec<Violation>, Vec<Fact>, bool, RuleTiming), RunError> {
        let FileUnderCheck {
            path,
            source,
            tree: _,
            language: _,
        } = *file;

        // The grammar is chosen by the file, not by the rule — the same gate the JavaScript
        // path applies, applied identically, so the two engines cannot disagree about which
        // files a rule runs on.
        let Some(language_id) = self.language_of(path) else {
            return Ok((Vec::new(), Vec::new(), false, RuleTiming::default()));
        };
        let Some((_, compiled_query)) = rule.for_language(language_id) else {
            return Ok((Vec::new(), Vec::new(), false, RuleTiming::default()));
        };

        let mut timing = RuleTiming::default();
        let clock = |on: bool| on.then(std::time::Instant::now);
        let query_started = clock(self.profiling);

        // Matches first, and the context only if there are any. A rule whose query matches
        // nothing on this file must not open a context, instantiate anything, or copy the
        // file's source into an arena.
        let precollected_is_empty = precollected.as_ref().is_some_and(Vec::is_empty);
        if precollected_is_empty {
            if let Some(started) = query_started {
                timing.query = started.elapsed();
            }
            return Ok((Vec::new(), Vec::new(), false, timing));
        }

        self.open_context(worker, pass, file)?;
        let Some(resource) = pass.context.as_ref() else {
            return Err(RunError::Worker {
                detail: "the file's component context was not opened".to_owned(),
            });
        };
        let runtime = worker.runtime()?;

        // Already matched, in one traversal shared with every other rule on this file — or not,
        // in which case this rule walks the tree alone.
        let matches = if let Some(found) = precollected {
            found
        } else {
            let host = runtime
                .host_mut()
                .check_context_mut(resource)
                .map_err(|e| Self::component_failure(rule, path, &e.to_string()))?;
            walk_for(host, compiled_query, source)
        };

        if let Some(started) = query_started {
            timing.query = started.elapsed();
            timing.matches = matches.len() as u64;
        }
        if matches.is_empty() {
            return Ok((Vec::new(), Vec::new(), false, timing));
        }

        let timeout = rule.spec.timeout.unwrap_or(self.limits.rule_timeout);
        for captures in matches {
            let entries: Vec<types::MatchEntry> = {
                let host = runtime
                    .host_mut()
                    .check_context_mut(resource)
                    .map_err(|e| Self::component_failure(rule, path, &e.to_string()))?;
                let arena = host.arena_mut();
                captures
                    .into_iter()
                    .filter_map(|(name, path)| {
                        arena
                            .intern_path(path)
                            .map(|node| types::MatchEntry { name, node })
                    })
                    .collect()
            };

            let handler_started = clock(self.profiling);
            let outcome = runtime.check_with_timeout(slot, resource, &entries, timeout);
            if let Some(started) = handler_started {
                timing.handler = timing.handler.saturating_add(started.elapsed());
            }
            outcome.map_err(|e: WasmError| Self::component_failure(rule, path, &e.to_string()))?;
        }

        // Taken per rule rather than per file, which is what attributes a report to the rule
        // that made it: the context is shared, and `take_reports` empties it.
        let host = runtime
            .host_mut()
            .check_context_mut(resource)
            .map_err(|e| Self::component_failure(rule, path, &e.to_string()))?;
        let reports = host.take_reports();
        let emitted = host.take_facts();

        let facts = emitted
            .into_iter()
            .enumerate()
            .map(|(sequence, fact)| Fact {
                rule_id: rule.spec.id.clone(),
                file: path.clone(),
                kind: fact.kind,
                // The payload exactly as the guest serialized it. **Nothing merges a `file`
                // key into it**, unlike the JavaScript path: `lanekeep-js`'s reduce phase
                // splices one in because its `ReduceFact` carries only `kind` and `json`,
                // where the world's `emitted-fact` has a `file` field of its own. Doing both
                // would put a literal duplicate `"file"` key in the payload a component reads.
                data: fact.data,
                sequence: u32::try_from(sequence).unwrap_or(u32::MAX),
            })
            .collect();

        // The date flag is sticky and belongs to the context, so it is read once when the file
        // is finished rather than claimed per rule — see `check_file`.
        Ok((
            Self::violations_from(rule, path, reports),
            facts,
            false,
            timing,
        ))
    }

    /// Turn a component's reports into violations, under the rule's own identity.
    ///
    /// Identical in shape to what [`Engine::run_rule`] does with `lanekeep_js::Report`, and
    /// deliberately so: a rule supplies a position and optionally a message, and the id,
    /// severity, remediation and default message come from the engine. That is what stops a
    /// rule reporting under someone else's name, and it must not depend on which engine ran it.
    fn violations_from(
        rule: &Prepared,
        path: &FilePath,
        reports: Vec<lanekeep_wasm::host::Report>,
    ) -> Vec<Violation> {
        reports
            .into_iter()
            .map(|report| Violation {
                rule_id: rule.spec.id.clone(),
                location: Location::new(path.clone(), Position::new(report.line, report.column)),
                message: report
                    .message
                    .unwrap_or_else(|| rule.spec.card.message.clone()),
                remediation: rule.spec.card.remediation.clone(),
                severity: rule.spec.severity,
                fix: report.fix,
            })
            .collect()
    }

    /// Open the file's component context, if this is the first rule that needs one.
    ///
    /// The resource stays in the caller's `Option` rather than being handed back by value:
    /// a `Resource` is an owned table entry, so two of them naming one rep would be two claims
    /// on the same context and a double delete when the file is finished.
    fn open_context(
        &self,
        worker: &mut Worker<'_>,
        pass: &mut ComponentPass<'_>,
        file: &FileUnderCheck<'_>,
    ) -> Result<(), RunError> {
        if pass.context.is_some() {
            return Ok(());
        }

        let mut built = CheckContext::new(
            lanekeep_js::NodeArena::new(file.tree.clone(), file.source.to_owned()),
            file.path.as_str(),
            Arc::clone(file.language),
        )
        .with_file_access(Arc::clone(pass.files))
        .with_today(&self.today.to_string());
        if let Some(resolver) = file.language.resolver() {
            built = built.with_resolver(resolver);
        }

        let resource = worker
            .runtime()?
            .host_mut()
            .push_check_context(built)
            .map_err(|e| RunError::Rule {
                rule: "<components>".to_owned(),
                file: file.path.as_str().to_owned(),
                detail: e.to_string(),
            })?;
        *pass.context = Some(resource);
        Ok(())
    }

    /// One shape for every way a component rule can fail on a file.
    fn component_failure(rule: &Prepared, path: &FilePath, detail: &str) -> RunError {
        RunError::Rule {
            rule: rule.spec.id.to_string(),
            file: path.as_str().to_owned(),
            detail: detail.to_owned(),
        }
    }

    fn run_rule(
        &self,
        worker: &mut Worker<'_>,
        files: &Arc<FileAccess>,
        rule: &Prepared,
        file: &FileUnderCheck<'_>,
        precollected: Option<RuleMatches>,
    ) -> Result<(Vec<Violation>, Vec<Fact>, bool, RuleTiming), RunError> {
        let FileUnderCheck {
            path,
            source,
            tree,
            language: _,
        } = *file;
        // The grammar is chosen by the file, not by the rule. A rule that does not target
        // this file's language does not run on it at all — previously it ran anyway, against
        // a grammar that could not parse the file, and matched nothing without saying so.
        let Some(language_id) = self.language_of(path) else {
            return Ok((Vec::new(), Vec::new(), false, RuleTiming::default()));
        };
        let Some((language, compiled_query)) = rule.for_language(language_id) else {
            return Ok((Vec::new(), Vec::new(), false, RuleTiming::default()));
        };

        // The tree is parsed once for the file and handed to every rule — §2's "run compiled
        // queries (one pass)" and §7's "single shared parse".
        //
        // It was parsed here instead, per rule, so a file admitted by twenty rules was parsed
        // twenty times. That is most of a cold run: the profile attributed it to query time,
        // which made twelve rules matching *nothing* look like they cost 400 ms of matching
        // each. `Tree::clone` is `ts_tree_copy`, a refcounted copy rather than a re-parse, so
        // each rule still gets an owned tree for its arena at almost no cost.
        let tree = tree.clone();

        // Only when asked. A clock read per invocation is cheap and not free, and this is
        // the hot path.
        let mut timing = RuleTiming::default();
        let clock = |on: bool| on.then(std::time::Instant::now);

        // Collect capture paths while the tree is borrowed, then intern once the borrow
        // has ended — the two-phase shape the arena's ownership of the tree forces.
        let mut matches: RuleMatches = Vec::new();

        let host = HostContext::new(tree, source.to_owned(), path.as_str())
            .with_resolver_from(language.as_ref())
            .with_language(Arc::clone(language))
            .with_today(&self.today.to_string())
            .with_file_access(Arc::clone(files));

        let query_started = clock(self.profiling);
        if let Some(found) = precollected {
            // Already matched, in one traversal shared with every other rule on this file.
            matches = found;
        } else {
            // No combined query for this language, or profiling asked for the per-rule
            // split. Walk the tree for this rule alone.
            let arena = host.arena().borrow();
            compiled_query.for_each_match(arena.tree(), source.as_bytes(), |m| {
                let captures = m
                    .captures
                    .iter()
                    .filter_map(|(name, node)| {
                        arena.path_of(*node).map(|path| ((*name).to_owned(), path))
                    })
                    .collect();
                matches.push(captures);
            });
        }

        if let Some(started) = query_started {
            timing.query = started.elapsed();
            timing.matches = matches.len() as u64;
        }

        if matches.is_empty() {
            return Ok((Vec::new(), Vec::new(), false, timing));
        }

        // Only now, with matches in hand, is a sandbox needed. Everything above — parsing,
        // query matching — is Rust, and a file that matches nothing never starts one.
        let sandbox = worker.sandbox()?;

        let timeout = rule.spec.timeout.unwrap_or(self.limits.rule_timeout);
        let mut violations = Vec::new();

        for captures in matches {
            let handles: Vec<(String, u32)> = {
                let mut arena = host.arena().borrow_mut();
                captures
                    .into_iter()
                    .filter_map(|(name, path)| arena.intern_path(path).map(|h| (name, h)))
                    .collect()
            };

            let literal = handles
                .iter()
                .map(|(name, handle)| format!("{}: {handle}", json_key(name)))
                .collect::<Vec<_>>()
                .join(", ");

            // The handler is invoked through the module the config already loaded, so the
            // rule object here is the same one the config validated.
            let call = format!(
                "globalThis.__lanekeepConfig.rules[{}].check(ctx, {{{literal}}})",
                rule_index(&rule.spec)
            );

            let handler_started = clock(self.profiling);
            let outcome = sandbox.eval_with_host_timeout::<()>(&host, &call, timeout);
            if let Some(started) = handler_started {
                timing.handler = timing.handler.saturating_add(started.elapsed());
            }

            outcome.map_err(|e: SandboxError| RunError::Rule {
                rule: rule.spec.id.to_string(),
                file: path.as_str().to_owned(),
                detail: e.to_string(),
            })?;
        }

        let facts = host
            .take_facts()
            .into_iter()
            .enumerate()
            .map(|(sequence, emitted)| Fact {
                rule_id: rule.spec.id.clone(),
                file: path.clone(),
                kind: emitted.kind,
                data: emitted.data,
                // Emission order within the file. The engine assigns it rather than
                // trusting the rule, so a rule cannot reorder its own facts relative to
                // another file's and change what `reduce` sees.
                sequence: u32::try_from(sequence).unwrap_or(u32::MAX),
            })
            .collect();

        for report in host.take_reports() {
            violations.push(Violation {
                rule_id: rule.spec.id.clone(),
                location: Location::new(path.clone(), Position::new(report.line, report.column)),
                message: report
                    .message
                    .unwrap_or_else(|| rule.spec.card.message.clone()),
                remediation: rule.spec.card.remediation.clone(),
                severity: rule.spec.severity,
                fix: report.fix,
            });
        }

        Ok((violations, facts, host.date_was_read(), timing))
    }
}

/// Whether a run looked at the whole corpus or a chosen subset.
///
/// The distinction only matters when saving: a run that saw everything may prune, and a run
/// that saw a subset must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coverage {
    /// Everything discovery selected.
    Whole,
    /// An explicit subset, from `--since` or `--staged`.
    Partial,
}

/// One rayon worker's reusable state.
///
/// The sandbox is built on first use rather than up front. Starting QuickJS and evaluating
/// every rule module into it costs real time, and a worker whose files all hit the cache —
/// or whose queries match nothing — never executes a line of JavaScript and does not need
/// one.
struct Worker<'a> {
    engine: &'a Engine,
    clock: Arc<RunClock>,
    sandbox: Option<Sandbox>,
    /// A failure to build, remembered so it is reported once per worker rather than
    /// retried for every remaining file.
    failed: Option<RunError>,
    /// This worker's component store, built on first use exactly as the sandbox is.
    ///
    /// **One store per worker holding one instance per rule — and rayon decides how many workers
    /// there are.** `lanekeep_wasm::WasmRuntime::for_rules` instantiates nothing (it allocates
    /// one `None` per rule), which is what makes it safe to build from rayon's initializer,
    /// since `map_init` runs that per *chunk* rather than per thread. Instantiation then happens
    /// in `WasmRuntime::rule`, at most once per slot per store.
    ///
    /// That is a bound per `Worker`, not per thread, and the difference is not small: measured
    /// through this engine at ten thousand files times ten rules, **1,038 stores and 10,380
    /// instantiations at fourteen threads**, varying between runs because rayon splits on how the
    /// work is going. `lanekeep_wasm::runtime::MEMORY_RESERVATION` used to be justified on
    /// "roughly three hundred and fifty instantiations, and it does not grow with the corpus";
    /// that half is false and its documentation now carries the re-derivation, the crossover, and
    /// why the constant is left where it is anyway.
    ///
    /// **The lever, if this ever needs bounding, is here rather than there.** `with_min_len` on
    /// `run_files`'s `par_iter` would cap the store count directly — and it is a bigger change
    /// than it looks, because this same initializer builds the QuickJS sandbox and one sandbox
    /// per chunk is the more expensive of the two. It would move the JavaScript path's measured
    /// behavior, so it needs a benchmark rather than an argument.
    wasm: Option<WasmRuntime>,
    /// The first component failure this worker saw, if it saw one.
    ///
    /// A trapped store cannot be entered again, so every file after the first failure would
    /// otherwise be reported with wasmtime's own bookkeeping message rather than with what
    /// actually went wrong. See [`Worker::poison_on`].
    poisoned: Option<RunError>,
}

impl<'a> Worker<'a> {
    fn new(engine: &'a Engine, clock: &Arc<RunClock>) -> Self {
        Self {
            engine,
            clock: Arc::clone(clock),
            sandbox: None,
            failed: None,
            wasm: None,
            poisoned: None,
        }
    }

    /// Remember a component failure, and hand it straight back.
    ///
    /// **A trap poisons the whole store, and the store outlives the file.** `bindgen!` is
    /// configured with `imports: { default: trappable }`, so any host refusal — and any guest
    /// trap — sets a store-wide flag with no public reset: a later, unrelated call on the same
    /// store fails with wasmtime's own `cannot enter component instance`, which names nothing
    /// that went wrong. Every such failure already cancels the run, so nothing is *rescued* by
    /// noticing; what is rescued is the diagnostic. rayon keeps handing this worker its
    /// remaining files, and which of several failures surfaces from the reduction is arbitrary,
    /// so without this the run can be reported against a file that was fine and a message that
    /// describes the runtime's bookkeeping rather than the rule.
    fn poison_on<T>(&mut self, outcome: &Result<T, RunError>) -> Result<T, RunError>
    where
        T: Clone,
    {
        match outcome {
            Ok(value) => Ok(value.clone()),
            Err(error) => {
                if self.poisoned.is_none() {
                    self.poisoned = Some(error.clone());
                }
                Err(error.clone())
            }
        }
    }

    /// This worker's component runtime, building it if this is the first component rule that
    /// needs one.
    ///
    /// A cached failure, as [`Worker::sandbox`] has one — but for the opposite reason. There it
    /// remembers a build that failed so the build is not retried per file; here it remembers a
    /// *store* that trapped, because the store cannot be used again and its own account of that
    /// is uninformative. See [`Worker::poison_on`].
    fn runtime(&mut self) -> Result<&mut WasmRuntime, RunError> {
        if let Some(error) = &self.poisoned {
            return Err(error.clone());
        }

        if self.wasm.is_none() {
            let components = self
                .engine
                .components
                .as_ref()
                .ok_or_else(|| RunError::Worker {
                    detail: "a component rule was dispatched in a run that loaded no components"
                        .to_owned(),
                })?;
            self.wasm = Some(WasmRuntime::for_rules(
                Arc::clone(components.engine()),
                Arc::clone(components.rules()),
                self.engine.limits,
                Arc::clone(&self.clock),
            ));
        }

        self.wasm.as_mut().ok_or_else(|| RunError::Worker {
            detail: "the component runtime was not built".to_owned(),
        })
    }

    /// This worker's sandbox, building it if this is the first rule that needs one.
    fn sandbox(&mut self) -> Result<&Sandbox, RunError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }

        if self.sandbox.is_none() {
            match self.engine.build_sandbox(&self.clock) {
                Ok(sandbox) => self.sandbox = Some(sandbox),
                Err(error) => {
                    self.failed = Some(error.clone());
                    return Err(error);
                }
            }
        }

        self.sandbox.as_ref().ok_or_else(|| RunError::Worker {
            detail: "sandbox was not built".to_owned(),
        })
    }
}

/// What checking one file produced.
struct FileOutcome {
    /// The file this is about, so the run can key dependencies by it.
    path: FilePath,
    violations: Vec<Violation>,
    facts: Vec<Fact>,
    /// What this file's rules read beyond it.
    reads: Vec<TrackedRead>,
    /// The file's suppression directives, for filtering reduce-phase violations.
    suppressions: Vec<suppression::Suppression>,
    /// Indices of the directives that silenced something.
    used_suppressions: Vec<u32>,
    /// Whether any rule read `ctx.today` while checking this file.
    read_the_date: bool,
    /// Per-rule timings, when profiling.
    timings: Vec<(RuleId, RuleTiming)>,
    /// What to store for this file, when caching is on.
    entry: Option<(CacheKey, CacheEntry)>,
    /// Whether the file was parsed at all, for the "n files checked" count.
    parsed: bool,
}

impl FileOutcome {
    /// A file that never reached a parser — gated out, unreadable, or not UTF-8.
    const fn skipped(path: FilePath) -> Self {
        Self {
            path,
            violations: Vec::new(),
            facts: Vec::new(),
            reads: Vec::new(),
            suppressions: Vec::new(),
            used_suppressions: Vec::new(),
            read_the_date: false,
            timings: Vec::new(),
            entry: None,
            parsed: false,
        }
    }

    const fn parsed(path: FilePath) -> Self {
        Self {
            path,
            violations: Vec::new(),
            facts: Vec::new(),
            reads: Vec::new(),
            suppressions: Vec::new(),
            used_suppressions: Vec::new(),
            read_the_date: false,
            timings: Vec::new(),
            entry: None,
            parsed: true,
        }
    }

    /// A file whose result came back from the cache.
    ///
    /// Counted as parsed, because from outside the run it was checked — reporting a warm
    /// run as having checked nothing would make the number useless.
    fn cached(path: FilePath, key: CacheKey, entry: CacheEntry) -> Self {
        Self {
            path,
            violations: entry.violations.clone(),
            facts: entry.facts.clone(),
            reads: entry.dependencies.clone(),
            suppressions: entry.suppressions.clone(),
            used_suppressions: entry.used_suppressions.clone(),
            // A cache hit ran no rules, so nothing read the date this time. Whether the
            // entry was dated is already settled by the key it was found under.
            read_the_date: false,
            timings: Vec::new(),
            entry: Some((key, entry)),
            parsed: true,
        }
    }

    /// A file that no rule's content gates admitted.
    fn empty_entry(path: FilePath, key: Option<CacheKey>) -> Self {
        Self {
            path,
            violations: Vec::new(),
            facts: Vec::new(),
            reads: Vec::new(),
            suppressions: Vec::new(),
            used_suppressions: Vec::new(),
            read_the_date: false,
            timings: Vec::new(),
            entry: key.map(|key| (key, CacheEntry::default())),
            parsed: false,
        }
    }
}

/// One file's directives, and which of them silenced something.
struct FileDirectives {
    suppressions: Vec<suppression::Suppression>,
    /// Indices into `suppressions`. Carried from the cache entry on a warm run.
    used: Vec<u32>,
}

/// Which directive silences a violation reported into some other file.
///
/// A cross-file rule reports at the site a fact came from, so the directives that matter are
/// that file's, not the one the rule happened to be reducing over.
fn covering_elsewhere(
    directives: &BTreeMap<FilePath, FileDirectives>,
    violation: &Violation,
) -> Option<(FilePath, u32)> {
    let found = directives.get(&violation.location.file)?;
    let index = found.suppressions.iter().position(|suppression| {
        suppression.covers(&violation.rule_id, violation.location.position.line)
    })?;

    Some((
        violation.location.file.clone(),
        u32::try_from(index).unwrap_or(u32::MAX),
    ))
}

/// Violations for directives that silenced nothing.
///
/// A suppression whose violation no longer exists is debt: it documents a decision about
/// code that has changed, and the next person to read it has no way to tell it is stale.
///
/// Reported as warnings rather than errors. Turning on a hygiene report should not fail a
/// build that was passing — the point is to show the debt, not to refuse to proceed until it
/// is paid.
fn unused_violations(directives: &BTreeMap<FilePath, FileDirectives>) -> Vec<Violation> {
    let Ok(rule_id) = SUPPRESSION_RULE.parse::<RuleId>() else {
        return Vec::new();
    };

    let mut violations = Vec::new();
    for (file, found) in directives {
        for (index, suppression) in found.suppressions.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            if found.used.contains(&index) {
                continue;
            }

            violations.push(Violation {
                rule_id: rule_id.clone(),
                location: Location::new(
                    file.clone(),
                    Position::new(suppression.line, suppression.column),
                ),
                message: format!("suppression silenced nothing — \"{}\"", suppression.reason),
                remediation: String::from(
                    "remove it: whatever it was accepting is no longer reported",
                ),
                severity: Severity::Warn,
                fix: None,
            });
        }
    }
    violations
}

/// Load, check and link every component-backed rule, filling in its slot.
///
/// Returns `None` when no rule names a component, and that is the case worth stating: building
/// a [`WasmEngine`] spawns the epoch ticker thread that enforces both wall-clock budgets, so a
/// run with no component rule — which is every run this tree can express today — must not build
/// one. Nothing is instantiated here either way; an instance belongs to a store and a store
/// belongs to a worker.
///
/// The order is the ruleset's, so a broken component is reported against the first rule in
/// config order that has one rather than against whichever load finished first.
///
/// # Errors
///
/// Returns [`RunError::Component`] when a component's bytes cannot be read, cannot be compiled,
/// reach for an import the sandbox does not permit, or do not satisfy the `rule` world; and
/// [`RunError::Worker`] when the runtime itself cannot be built or has bound something the
/// cache key does not know about.
fn load_components(
    rules: &mut [Prepared],
    project_root: &Path,
) -> Result<Option<Components>, RunError> {
    if rules.iter().all(|rule| rule.spec.component.is_none()) {
        return Ok(None);
    }

    let engine = WasmEngine::new().map_err(|e: WasmError| RunError::Worker {
        detail: e.to_string(),
    })?;
    let mut set = RuleSet::new(&engine).map_err(|e| RunError::Worker {
        detail: e.to_string(),
    })?;

    // Writes precompiled artifacts under the project's own `.lanekeep/components`, and falls
    // back to compiling in-process when that is not writable. Compiling twenty components costs
    // about 186 ms against about 0.74 ms to map twenty precompiled ones, which is 23% of the
    // whole cold budget spent before a file is read.
    let loader = ComponentLoader::for_project_root(project_root);

    for rule in rules.iter_mut() {
        let Some(path) = rule.spec.component.clone() else {
            continue;
        };
        let name = rule.spec.id.to_string();
        let bytes = std::fs::read(&path).map_err(|e| RunError::Component {
            rule: name.clone(),
            detail: format!("cannot read `{}`: {e}", path.display()),
        })?;
        let admitted = loader
            .load(&engine, &name, &bytes)
            .map_err(|e: WasmError| RunError::Component {
                rule: name.clone(),
                detail: e.to_string(),
            })?;
        let slot = set.add(&name, &admitted).map_err(|e| RunError::Component {
            rule: name,
            detail: e.to_string(),
        })?;
        rule.slot = Some(slot);
    }

    Components::linked(engine, set).map(Some)
}

/// Refuse a run that bound an interface the cache key was not computed against.
///
/// **The half of `EXTERNAL_BINDINGS` that was a signature and is now a check.**
/// `RuleSet::linker_mut` takes an [`ExternalBinding`], so nothing reaches the linker without
/// naming what fixes its answers — but nothing compared that declaration against
/// [`EXTERNAL_BINDINGS`], which is the list `lanekeep_wasm::host_api_hash` actually folds into
/// the key. A binding made at the call site and left out of the constant is a run whose rules
/// can reach something no cached result knows about, with every key identical.
///
/// It could not be closed in `lanekeep-wasm`: the key is computed when a configuration is
/// loaded, before any `RuleSet` exists. It closes here because this is the first place that
/// holds both — a linked set, and the constant the key was built from.
///
/// Both lists are empty today and the comparison is exact, including order: a declaration is a
/// cache-key input, and two runs binding the same interfaces in different orders fold to
/// different hashes, so accepting them as equal here would be accepting a key mismatch.
fn declared_bindings_match(bound: &[ExternalBinding]) -> Result<(), RunError> {
    if bound == EXTERNAL_BINDINGS {
        return Ok(());
    }

    let render = |bindings: &[ExternalBinding]| {
        if bindings.is_empty() {
            return "nothing".to_owned();
        }
        bindings
            .iter()
            .map(|b| format!("`{}` ({})", b.interface(), b.behavior()))
            .collect::<Vec<_>>()
            .join(", ")
    };

    Err(RunError::Worker {
        detail: format!(
            "this run bound {} beside the declared world, and the cache key was computed \
             against {}\n  \
             a bound interface is a cache-key input: add it to `lanekeep_wasm::EXTERNAL_BINDINGS` \
             so a result computed without it is not served to a run that has it",
            render(bound),
            render(EXTERNAL_BINDINGS),
        ),
    })
}

/// Everything about a run that every file's key shares.
///
/// A named function rather than a call inside [`Engine::prepare`], because it is the one place
/// the five run-wide inputs are actually assembled and a value dropped here is dropped from
/// every key in the run. Inline it and "the compilation environment reaches a real run's key"
/// becomes a claim about a private field of a struct that needs a project on disk to build.
///
/// # Errors
///
/// Returns [`RunError::WasmRuntime`] when `wasmtime` cannot describe its own compilation
/// environment on this host.
fn run_key(
    ruleset_hash: &[u8],
    config_hash: &[u8],
    grammars: &[GrammarKey],
) -> Result<RunKey, RunError> {
    let compile_env = lanekeep_wasm::compile_env_hash().map_err(|e| RunError::WasmRuntime {
        detail: e.to_string(),
    })?;

    Ok(RunKey::new(
        // Major.minor only: a patch release changes nothing a rule can observe, and
        // invalidating every cache on one would make patch upgrades expensive for nothing.
        engine_version(),
        &host_api_hash(),
        &compile_env,
        ruleset_hash,
        config_hash,
        grammars,
    ))
}

/// Everything a rule may reach, from both engines, in one cache-key field.
///
/// This crate is where the two host surfaces meet, so it is where they are folded. QuickJS's
/// `ctx` is still a hand-maintained `u32` — `lanekeep_js::HOST_API_VERSION`, whose own
/// documentation says nothing detects a missed bump — and a component's surface is
/// [`lanekeep_wasm::host_api_hash`], a content hash of the WIT file every binding is generated
/// from plus whatever the host binds beside that world.
///
/// **Both, and not the newer one instead of the older.** Every rule in this tree is still
/// TypeScript, so dropping the `ctx` version would take the only host surface a run actually
/// uses out of the key: adding a `ctx` function would then serve results computed by a build
/// where it did not exist, which is the failure the field exists to prevent. The `u32` leaves
/// with the last JavaScript rule and not before.
fn host_api_hash() -> [u8; 32] {
    fold_host_api(HOST_API_VERSION, &lanekeep_wasm::host_api_hash())
}

/// The fold, separated from its inputs so a test can vary them.
///
/// `HOST_API_VERSION` is a `const` and the WIT hash is derived, so neither can be moved in a
/// test against the real function — and "both halves are in the key" is exactly the claim that
/// is worth nothing unasserted. This is the same reasoning `lanekeep_wasm::key` uses for its
/// two folds.
fn fold_host_api(ctx_version: u32, wasm_world: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-host-api");
    hasher.update(&ctx_version.to_le_bytes());
    hasher.update(wasm_world);
    *hasher.finalize().as_bytes()
}

/// The engine version a cache key uses: major.minor only.
fn engine_version() -> &'static str {
    // Trimmed at the second dot. A patch release changes nothing a rule can observe, so
    // invalidating every cache in the world on one would cost users time for nothing.
    const FULL: &str = env!("CARGO_PKG_VERSION");
    match FULL.match_indices('.').nth(1) {
        Some((at, _)) => FULL.split_at(at).0,
        None => FULL,
    }
}

/// The id violations about suppressions are reported under.
///
/// A real namespaced id, so it sorts, suppresses and serializes like any other — and so a
/// consumer parsing output does not meet a special case.
const SUPPRESSION_RULE: &str = "lanekeep/suppression";

/// Position of a rule in the config's `rules` array, which is how the handler is reached.
fn rule_index(spec: &RuleSpec) -> usize {
    spec.index
}

/// Quote a capture name for use as an object key.
fn json_key(name: &str) -> String {
    format!("{name:?}")
}

/// Convenience for callers that only need a default severity check.
#[must_use]
pub fn any_failing(violations: &[Violation]) -> bool {
    violations.iter().any(|v| v.severity == Severity::Error)
}

/// Where the rules root sits, given a project root.
#[must_use]
pub fn rules_root_for(project_root: &Path) -> PathBuf {
    project_root.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use lanekeep_lang_js::{JavaScript, TypeScript};

    use super::*;

    #[test]
    fn both_host_surfaces_reach_the_cache_key() {
        // Two engines, one field. A change to either has the same consequence — a rule could
        // not have called something that did not exist — so a fold that dropped one would
        // serve stale results for exactly the rules that engine runs.
        let base = fold_host_api(1, b"world");
        assert_ne!(
            base,
            fold_host_api(2, b"world"),
            "a `ctx` function added to QuickJS must invalidate"
        );
        assert_ne!(
            base,
            fold_host_api(1, b"a-wider-world"),
            "a function added to the WIT world must invalidate"
        );
    }

    #[test]
    fn the_host_api_fold_reads_the_real_world_and_the_real_ctx_version() {
        // The fold is only worth testing if the shipped call feeds it the shipped values.
        assert_eq!(
            host_api_hash(),
            fold_host_api(HOST_API_VERSION, &lanekeep_wasm::host_api_hash())
        );
    }

    #[test]
    fn the_two_wasm_inputs_reach_a_real_runs_key() {
        // Both of the new fields, asserted at the place they are assembled rather than at
        // `RunKey`'s door. A hash that is derived correctly and then not passed is the same
        // stale-answer bug as one that is never derived, and the tests either side of this one
        // pass against exactly that.
        let grammars = [GrammarKey {
            id: "typescript".to_owned(),
            abi: 15,
        }];
        let content = lanekeep_core::ContentHash::new([7; 32]);
        let real = run_key(b"ruleset", b"config", &grammars).expect("the runtime describes itself");

        for (label, host_api, compile_env) in [
            (
                "the WebAssembly compilation environment",
                host_api_hash().to_vec(),
                Vec::new(),
            ),
            (
                "the host API surface",
                Vec::new(),
                lanekeep_wasm::compile_env_hash()
                    .expect("the runtime describes itself")
                    .to_vec(),
            ),
        ] {
            let without = RunKey::new(
                engine_version(),
                &host_api,
                &compile_env,
                b"ruleset",
                b"config",
                &grammars,
            );
            assert_ne!(
                real.for_file("src/a.ts", &content),
                without.for_file("src/a.ts", &content),
                "{label} must reach the key a run actually files results under"
            );
        }
    }

    struct Project {
        dir: PathBuf,
    }

    impl Project {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("lanekeep-engine-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("creates dir");
            let project = Self { dir };
            for (path, contents) in files {
                project.write(path, contents);
            }
            project
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("creates parent");
            }
            fs::write(full, contents).expect("writes");
        }

        fn run(&self) -> Result<Outcome, RunError> {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            let config_path = self.dir.join("lanekeep.config.ts");

            let sandbox =
                lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                    .expect("sandbox");
            let config = lanekeep_config::load(&sandbox, &root, &config_path)
                .unwrap_or_else(|e| panic!("config failed to load: {e}"));

            let engine = Engine::prepare(
                &config,
                &self.dir,
                root,
                &config_path,
                &lanekeep_lang_js::registry(),
                Arc::new(TypeScript),
                Arc::new(JavaScript),
            )?;
            engine.run()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// A rule reporting every `debugger` statement — small, unambiguous, and easy to seed.
    const DEBUGGER_RULE: &str = "import { defineRule } from 'lanekeep';\n\
        export default defineRule({\n\
          id: 'local/no-debugger',\n\
          query: '(debugger_statement) @stmt',\n\
          card: {\n\
            message: 'debugger statement',\n\
            remediation: 'remove it before committing',\n\
            examples: { bad: 'debugger;', good: 'console.log(x);' },\n\
          },\n\
          check(ctx, m) { ctx.report(m.stmt); },\n\
        });\n";

    /// A rule matching `x.y`, which is the shape that vanishes when JSX fails to parse.
    fn member_rule_for(language: &str) -> String {
        let declaration = if language.is_empty() {
            String::new()
        } else {
            format!("  language: {language},\n")
        };
        format!(
            "import {{ defineRule }} from 'lanekeep';\n\
             export default defineRule({{\n\
               id: 'local/member',\n\
             {declaration}\
               query: '(member_expression) @m',\n\
               card: {{\n\
                 message: 'member expression',\n\
                 remediation: 'n/a',\n\
                 examples: {{ bad: 'a.b', good: 'b' }},\n\
               }},\n\
               check(ctx, m) {{ ctx.report(m.m); }},\n\
             }});\n"
        )
    }

    fn config_for(include: &str) -> String {
        format!(
            "import {{ defineConfig }} from 'lanekeep';\n\
             import rule from './rule';\n\
             export default defineConfig({{ include: ['{include}'], rules: [rule] }});\n"
        )
    }

    fn config(extra: &str) -> String {
        format!(
            "import {{ defineConfig }} from 'lanekeep';\n\
             import rule from './rule';\n\
             export default defineConfig({{ include: ['src/**/*.ts'], rules: [rule]{extra} }});\n"
        )
    }

    /// A rule with no `language` of its own has to see inside JSX.
    ///
    /// The default used to be `typescript` alone, and the engine parsed every file with the
    /// rule's grammar whatever the file was. So a `.tsx` file went through the TypeScript
    /// grammar, every JSX element became an `ERROR` node, and a query simply matched nothing
    /// inside it — with no error, no warning, and no way to tell from the output. On a React
    /// codebase that is most of the code.
    #[test]
    fn a_default_rule_sees_inside_jsx() {
        let project = Project::new(
            "jsx-default",
            &[
                ("rule.ts", &member_rule_for("")),
                ("lanekeep.config.ts", &config_for("src/**/*.tsx")),
                (
                    "src/Component.tsx",
                    "export const C = () => <View style={styles.used} />;\n",
                ),
            ],
        );

        let outcome = project.run().expect("runs");

        assert_eq!(
            outcome.violations.len(),
            1,
            "a member expression inside JSX was not seen: {:?}",
            outcome.violations
        );
    }

    /// And the same rule still works on plain TypeScript, each file through its own grammar.
    #[test]
    fn a_default_rule_still_sees_plain_typescript() {
        let project = Project::new(
            "ts-default",
            &[
                ("rule.ts", &member_rule_for("")),
                ("lanekeep.config.ts", &config_for("src/**/*.ts")),
                ("src/plain.ts", "const x = styles.used;\n"),
            ],
        );

        let outcome = project.run().expect("runs");

        assert_eq!(outcome.violations.len(), 1, "{:?}", outcome.violations);
    }

    /// A rule that names one language is not run on files belonging to another.
    ///
    /// Previously it was run on everything and the mismatch showed up as an unparsable tree
    /// rather than as a skip, which is the failure this whole change is about.
    #[test]
    fn a_rule_does_not_run_on_a_language_it_does_not_name() {
        let project = Project::new(
            "single-language",
            &[
                ("rule.ts", &member_rule_for("'typescript'")),
                ("lanekeep.config.ts", &config_for("src/**/*.tsx")),
                (
                    "src/Component.tsx",
                    "export const C = () => <View style={styles.used} />;\n",
                ),
            ],
        );

        let outcome = project.run().expect("runs");

        assert!(
            outcome.violations.is_empty(),
            "a typescript-only rule ran on a tsx file: {:?}",
            outcome.violations
        );
    }

    /// Naming several languages runs the rule against each, compiled per grammar.
    #[test]
    fn a_rule_may_name_several_languages() {
        let project = Project::new(
            "many-languages",
            &[
                ("rule.ts", &member_rule_for("['typescript', 'tsx']")),
                ("lanekeep.config.ts", &config_for("src/**/*.{ts,tsx}")),
                ("src/plain.ts", "const x = styles.used;\n"),
                (
                    "src/Component.tsx",
                    "export const C = () => <View style={styles.used} />;\n",
                ),
            ],
        );

        let outcome = project.run().expect("runs");

        assert_eq!(outcome.violations.len(), 2, "{:?}", outcome.violations);
    }

    /// An unknown language is still an error, however it is spelled.
    #[test]
    fn an_unknown_language_in_a_list_is_reported() {
        let project = Project::new(
            "unknown-in-list",
            &[
                ("rule.ts", &member_rule_for("['typescript', 'klingon']")),
                ("lanekeep.config.ts", &config_for("src/**/*.ts")),
                ("src/plain.ts", "const x = styles.used;\n"),
            ],
        );

        let error = project
            .run()
            .expect_err("should refuse an unknown language");
        assert!(
            error.to_string().contains("klingon"),
            "the error should name it: {error}"
        );
    }

    #[test]
    fn runs_a_rule_over_a_corpus_end_to_end() {
        let project = Project::new(
            "end-to-end",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/clean.ts", "const a = 1;\n"),
                ("src/dirty.ts", "const b = 2;\ndebugger;\n"),
                ("src/also.ts", "function f() {\n  debugger;\n}\n"),
            ],
        );

        let outcome = project.run().expect("runs");

        assert_eq!(outcome.violations.len(), 2, "{:?}", outcome.violations);
        let rendered: Vec<String> = outcome
            .violations
            .iter()
            .map(|v| format!("{} {}", v.rule_id, v.location))
            .collect();
        assert_eq!(
            rendered,
            [
                "local/no-debugger src/also.ts:2:3",
                "local/no-debugger src/dirty.ts:2:1",
            ]
        );
        assert_eq!(outcome.violations[0].message, "debugger statement");
        assert_eq!(
            outcome.violations[0].remediation,
            "remove it before committing"
        );
    }

    #[test]
    fn output_is_identical_across_repeated_runs() {
        // The guarantee the whole design rests on. Files are checked in parallel, so
        // violations arrive in an order that varies run to run; only the sort makes the
        // output stable, and it has to hold across many files rather than two.
        let mut files = vec![
            ("rule.ts".to_owned(), DEBUGGER_RULE.to_owned()),
            ("lanekeep.config.ts".to_owned(), config("")),
        ];
        for i in 0..40 {
            files.push((
                format!("src/f{i}.ts"),
                format!("const x{i} = 1;\ndebugger;\n"),
            ));
        }
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let project = Project::new("determinism", &borrowed);

        let first = project.run().expect("runs").violations;
        assert_eq!(first.len(), 40);

        for _ in 0..4 {
            assert_eq!(project.run().expect("runs").violations, first);
        }
    }

    #[test]
    fn exclude_keeps_files_out_of_the_run() {
        let project = Project::new(
            "exclude",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config(", exclude: ['**/*.test.ts']")),
                ("src/a.ts", "debugger;\n"),
                ("src/a.test.ts", "debugger;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert_eq!(outcome.violations[0].location.file.as_str(), "src/a.ts");
    }

    #[test]
    fn a_content_gate_skips_the_parse() {
        // The gate's whole purpose. `files_parsed` is what proves it skipped rather than
        // parsed and found nothing — the violation count would look identical either way.
        let gated = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/no-debugger',\n\
              query: '(debugger_statement) @stmt',\n\
              gates: { fileContains: ['debugger'] },\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check(ctx, m) { ctx.report(m.stmt); },\n\
            });\n";

        let project = Project::new(
            "gate",
            &[
                ("rule.ts", gated),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
                ("src/b.ts", "const b = 1;\n"),
                ("src/c.ts", "const c = 2;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.files_discovered, 3);
        assert_eq!(
            outcome.files_parsed, 1,
            "only the file containing the needle should parse"
        );
        assert_eq!(outcome.violations.len(), 1);
    }

    #[test]
    fn a_rule_set_to_off_does_not_run() {
        let project = Project::new(
            "off",
            &[
                ("rule.ts", DEBUGGER_RULE),
                (
                    "lanekeep.config.ts",
                    &config(", severity: { 'local/no-debugger': 'off' }"),
                ),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
    }

    #[test]
    fn severity_reaches_the_violation() {
        let project = Project::new(
            "severity",
            &[
                ("rule.ts", DEBUGGER_RULE),
                (
                    "lanekeep.config.ts",
                    &config(", severity: { 'local/no-debugger': 'warn' }"),
                ),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations[0].severity, Severity::Warn);
        assert!(!any_failing(&outcome.violations));
    }

    #[test]
    fn a_rule_that_throws_aborts_the_run_naming_itself_and_the_file() {
        // §6.8: a breach cancels rather than degrading to a partial result, and the
        // diagnostic has to identify the culprit or it is not actionable.
        let throwing = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/throws',\n\
              query: '(debugger_statement) @stmt',\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check() { throw new Error('rule bug'); },\n\
            });\n";

        let project = Project::new(
            "throws",
            &[
                ("rule.ts", throwing),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );

        let err = project.run().expect_err("must abort");
        let rendered = err.to_string();
        assert!(rendered.contains("local/throws"), "{rendered}");
        assert!(rendered.contains("src/a.ts"), "{rendered}");
        assert!(rendered.contains("rule bug"), "{rendered}");
    }

    #[test]
    fn an_invalid_query_fails_before_any_file_is_read() {
        let bad = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/bad-query',\n\
              query: '(no_such_node) @x',\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check() {},\n\
            });\n";

        let project = Project::new(
            "bad-query",
            &[
                ("rule.ts", bad),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );

        let err = project.run().expect_err("must fail at preparation");
        assert!(matches!(err, RunError::Query { .. }), "{err:?}");
        assert!(err.to_string().contains("no_such_node"), "{err}");
    }

    #[test]
    fn a_file_is_parsed_once_however_many_rules_run_on_it() {
        // §2's "run compiled queries (one pass)" and §7's "single shared parse", asserted
        // structurally because the cost of breaking it is invisible: parsing per rule
        // produces identical output and simply runs N times slower. It did exactly that
        // until measured — a file admitted by twenty rules was parsed twenty times, which
        // was most of a cold run and showed up in the profile as *query* time, making rules
        // that matched nothing look expensive to match.
        //
        // Counting parses at run time would need a hook through the hot path for a test.
        // Reading this file is cheaper and catches the same regression: the only way back is
        // to construct a parser inside the per-rule path again.
        let source = include_str!("lib.rs");
        let body = source.split("#[cfg(test)]").next().unwrap_or(source);
        let parsers = body.matches("tree_sitter::Parser::new()").count();

        assert_eq!(
            parsers, 1,
            "the engine constructs {parsers} tree-sitter parsers outside tests; there must be \
             exactly one, in `check_file`, shared by every rule that runs on the file"
        );
    }

    #[test]
    fn the_combined_and_per_rule_paths_report_the_same_thing() {
        // There are two ways to match a file — one traversal for every rule, or one per
        // rule — and `--profile` is what chooses between them. Two paths through the hot
        // path is a place for divergence, and divergence here is silent: a rule whose
        // matches were handed to the wrong owner, or dropped, reports fewer violations and
        // nothing says so.
        //
        // Rules deliberately share capture names and node kinds. A combined query numbers
        // captures across the whole query rather than per pattern, so `@stmt` meaning one
        // thing in the third rule and another in the fifth is exactly the confusion an
        // owner map has to survive.
        let rule = |id: &str, query: &str| {
            format!(
                "import {{ defineRule }} from 'lanekeep';\n\
                 export default defineRule({{\n\
                 \x20 id: 'local/{id}',\n\
                 \x20 severity: 'error',\n\
                 \x20 card: {{ message: '{id}', remediation: 'n/a', \
                 examples: {{ bad: 'a', good: 'b' }} }},\n\
                 \x20 query: '{query}',\n\
                 \x20 check(ctx, m) {{ if (m.stmt) ctx.report(m.stmt); }},\n\
                 }});\n"
            )
        };

        let project = Project::new(
            "both-paths",
            &[
                // Two patterns, and first, so pattern indices stop coinciding with rule
                // indices. With one pattern per rule the identity map is accidentally
                // correct, and a test built that way passes against an engine that ignores
                // the owner map entirely — which is how the first version of this test was
                // written, and it did.
                (
                    "rules/a.ts",
                    &rule(
                        "a",
                        "(debugger_statement) @stmt (lexical_declaration) @stmt",
                    ),
                ),
                ("rules/b.ts", &rule("b", "(class_declaration) @stmt")),
                ("rules/c.ts", &rule("c", "(debugger_statement) @stmt")),
                (
                    "rules/d.ts",
                    &rule("d", "(call_expression function: (identifier) @fn) @stmt"),
                ),
                (
                    "lanekeep.config.ts",
                    "import { defineConfig } from 'lanekeep';\n\
                     import a from './rules/a';\n\
                     import b from './rules/b';\n\
                     import c from './rules/c';\n\
                     import d from './rules/d';\n\
                     export default defineConfig({\n\
                     \x20 include: ['src/**/*.ts'],\n\
                     \x20 rules: [a, b, c, d],\n\
                     });\n",
                ),
                (
                    "src/one.ts",
                    "export class A {\n  go() {\n    debugger;\n    helper();\n  }\n}\n",
                ),
                (
                    "src/two.ts",
                    "export class B {}\nexport function f() {\n  other();\n  debugger;\n}\n",
                ),
                ("src/three.ts", "export const n = 1;\n"),
            ],
        );

        let combined = project
            .build()
            .map(Engine::without_cache)
            .expect("engine")
            .run()
            .expect("combined run");
        let per_rule = project
            .build()
            .map(Engine::without_cache)
            .expect("engine")
            .profiling()
            .run()
            .expect("per-rule run");

        assert_eq!(
            rendered(&combined),
            rendered(&per_rule),
            "the shared traversal and the per-rule queries disagree"
        );
        // Not vacuous: a pair of empty runs would compare equal and assert nothing.
        assert!(
            combined.violations.len() >= 6,
            "the fixture should produce violations from several rules, got {}",
            combined.violations.len()
        );
    }

    #[test]
    fn every_pattern_in_a_combined_query_is_owned_by_the_rule_that_wrote_it() {
        // The map from pattern to rule is positional, so it is only correct if tree-sitter
        // numbers patterns in the order they were concatenated. Asserted directly, because
        // an off-by-one here does not fail — it hands one rule's matches to its neighbor,
        // and both rules keep reporting.
        let project = Project::new(
            "owner-map",
            &[
                // Two patterns in one rule, so the mapping cannot be one entry per rule.
                (
                    "rules/two.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     export default defineRule({\n\
                     \x20 id: 'local/two',\n\
                     \x20 severity: 'error',\n\
                     \x20 card: { message: 'two', remediation: 'n/a', \
                     examples: { bad: 'a', good: 'b' } },\n\
                     \x20 query: '(debugger_statement) @stmt (class_declaration) @stmt',\n\
                     \x20 check(ctx, m) { if (m.stmt) ctx.report(m.stmt); },\n\
                     });\n",
                ),
                (
                    "rules/one.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     export default defineRule({\n\
                     \x20 id: 'local/one',\n\
                     \x20 severity: 'error',\n\
                     \x20 card: { message: 'one', remediation: 'n/a', \
                     examples: { bad: 'a', good: 'b' } },\n\
                     \x20 query: '(function_declaration) @stmt',\n\
                     \x20 check(ctx, m) { if (m.stmt) ctx.report(m.stmt); },\n\
                     });\n",
                ),
                (
                    "lanekeep.config.ts",
                    "import { defineConfig } from 'lanekeep';\n\
                     import two from './rules/two';\n\
                     import one from './rules/one';\n\
                     export default defineConfig({\n\
                     \x20 include: ['src/**/*.ts'],\n\
                     \x20 rules: [two, one],\n\
                     });\n",
                ),
                ("src/a.ts", "export class C {}\n"),
            ],
        );

        let engine = project.build().expect("engine");
        let combined = combine_queries(&engine.rules);
        let combined = combined
            .get("typescript")
            .expect("typescript has a combined query");

        // Three patterns: two from the first rule, one from the second, in that order.
        assert_eq!(combined.owners, vec![0, 0, 1], "{:?}", combined.owners);
        assert_eq!(
            combined.query().expect("compiles").pattern_count(),
            combined.owners.len()
        );
    }

    #[test]
    fn two_broken_queries_always_name_the_same_rule() {
        // Queries compile in parallel, so which thread finishes first is not fixed. The
        // reported error must be the first by *config order* regardless — a project whose
        // rules are both broken must not be told about a different one each run, because
        // "fix that rule" followed by an error about another one reads as the tool being
        // wrong rather than as two problems.
        let broken = |name: &str| {
            format!(
                "import {{ defineRule }} from 'lanekeep';\n\
                 export default defineRule({{\n\
                   id: 'local/{name}',\n\
                   query: '(no_such_node_{name}) @x',\n\
                   card: {{ message: 'm', remediation: 'r', examples: {{ bad: 'a', good: 'b' }} }},\n\
                   check() {{}},\n\
                 }});\n"
            )
        };

        let config = "import { defineConfig } from 'lanekeep';\n\
             import first from './first';\n\
             import second from './second';\n\
             export default defineConfig({ include: ['src/**/*.ts'], rules: [first, second] });\n";

        // Repeated, because a race reported once is a race that passes sometimes.
        for attempt in 0..12 {
            let project = Project::new(
                &format!("two-broken-{attempt}"),
                &[
                    ("first.ts", &broken("first")),
                    ("second.ts", &broken("second")),
                    ("lanekeep.config.ts", config),
                    ("src/a.ts", "debugger;\n"),
                ],
            );

            let err = project.run().expect_err("must fail at preparation");
            assert!(
                err.to_string().contains("no_such_node_first"),
                "attempt {attempt} named the wrong rule: {err}"
            );
        }
    }

    #[test]
    fn a_rule_can_use_the_host_api_it_was_given() {
        // Proves the ctx surface is actually reachable from a real rule, not just from
        // the sandbox's own tests.
        let rule = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/long-names',\n\
              query: '(variable_declarator name: (identifier) @name)',\n\
              card: { message: 'name too long', remediation: 'shorten it', examples: { bad: 'a', good: 'b' } },\n\
              check(ctx, m) {\n\
                if (ctx.text(m.name).length > 5) ctx.report(m.name, `\\\"${ctx.text(m.name)}\\\" is too long`);\n\
              },\n\
            });\n";

        let project = Project::new(
            "host-api",
            &[
                ("rule.ts", rule),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const ok = 1;\nconst wayTooLong = 2;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert!(
            outcome.violations[0].message.contains("wayTooLong"),
            "{:?}",
            outcome.violations[0]
        );
    }

    #[test]
    fn a_corpus_with_no_matches_produces_nothing() {
        let project = Project::new(
            "clean",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
            ],
        );
        let outcome = project.run().expect("runs");
        assert!(outcome.violations.is_empty());
        assert_eq!(outcome.files_parsed, 1, "no gates means it is still parsed");
    }

    // --- the reduce phase ----------------------------------------------------------------

    /// A cross-file rule: every exported symbol nobody imports.
    ///
    /// The smallest rule that genuinely cannot work per-file — whether an export is unused
    /// is not a property of the file that declares it.
    const UNUSED_EXPORTS_RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/no-unused-exports',
  query: `
    (export_statement declaration: (function_declaration name: (identifier) @name)) @stmt
    (import_statement (import_clause (named_imports (import_specifier name: (identifier) @imported))))
  `,
  card: {
    message: 'unused export',
    remediation: 'delete it, or import it somewhere',
    examples: { bad: 'export function unused() {}', good: 'function used() {}' },
  },
  check(ctx, m) {
    if (m.imported) {
      ctx.emitFact({ kind: 'import', symbol: ctx.text(m.imported) });
      return;
    }
    ctx.emitFact({
      kind: 'export',
      symbol: ctx.text(m.name),
      line: ctx.line(m.stmt),
      column: ctx.column(m.stmt),
    });
  },
  reduce(ctx) {
    const imported = new Set(ctx.facts('import').map((f) => f.symbol));
    for (const e of ctx.facts('export')) {
      if (!imported.has(e.symbol)) {
        ctx.report({ file: e.file, line: e.line, column: e.column }, `'${e.symbol}' is exported but never imported`);
      }
    }
  },
});
";

    #[test]
    fn a_reduce_phase_sees_facts_from_every_file() {
        let project = Project::new(
            "reduce-cross-file",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "export function used() {}\nexport function spare() {}\n",
                ),
                ("src/b.ts", "import { used } from './a';\nused();\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        let found: Vec<(&str, u32, &str)> = outcome
            .violations
            .iter()
            .map(|v| {
                (
                    v.location.file.as_str(),
                    v.location.position.line,
                    v.message.as_str(),
                )
            })
            .collect();

        assert_eq!(
            found,
            vec![("src/a.ts", 2, "'spare' is exported but never imported")],
            "only the export nobody imports should be reported"
        );
    }

    #[test]
    fn a_rule_with_no_reduce_still_runs() {
        // The common path must not regress: no reduce phase, no sandbox built for one.
        let project = Project::new(
            "reduce-absent",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
    }

    #[test]
    fn a_reduce_phase_with_no_facts_reports_nothing() {
        let project = Project::new(
            "reduce-empty",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
    }

    #[test]
    fn the_file_list_reaches_the_reduce_phase() {
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/counts-files',
  query: '(debugger_statement) @stmt',
  card: {
    message: 'file count',
    remediation: 'nothing to do',
    examples: { bad: 'a', good: 'b' },
  },
  check() {},
  reduce(ctx) {
    ctx.report({ file: ctx.files[0], line: ctx.files.length, column: 1 });
  },
});
";
        let project = Project::new(
            "reduce-files",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
                ("src/b.ts", "const b = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        // Discovery sorts, so `files[0]` is `src/a.ts` on every run and every platform.
        assert_eq!(outcome.violations[0].location.file.as_str(), "src/a.ts");
        assert_eq!(outcome.violations[0].location.position.line, 2);
    }

    #[test]
    fn a_rule_does_not_see_another_rules_facts() {
        // Otherwise a payload shape becomes a contract between rules, and the result starts
        // depending on the order rules were declared in.
        const EMITTER: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/emitter',
  query: '(export_statement) @stmt',
  card: { message: 'emitter', remediation: 'x', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) { ctx.emitFact({ kind: 'thing', from: 'emitter' }); },
});
";
        const READER: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/reader',
  query: '(export_statement) @stmt',
  card: { message: 'reader', remediation: 'x', examples: { bad: 'a', good: 'b' } },
  check() {},
  reduce(ctx) {
    ctx.report({ file: 'seen.ts', line: ctx.facts().length + 1, column: 1 });
  },
});
";
        let project = Project::new(
            "reduce-isolation",
            &[
                ("emitter.ts", EMITTER),
                ("reader.ts", READER),
                (
                    "lanekeep.config.ts",
                    "import { defineConfig } from 'lanekeep';\n\
                     import emitter from './emitter';\n\
                     import reader from './reader';\n\
                     export default defineConfig({ include: ['src/**/*.ts'], rules: [emitter, reader] });\n",
                ),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert_eq!(
            outcome.violations[0].location.position.line, 1,
            "the reader saw the emitter's facts"
        );
    }

    #[test]
    fn a_reduce_phase_that_throws_aborts_the_run() {
        // Same posture as a `check` that throws: a partial result reported as a complete
        // one is worse than no result.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/throws-in-reduce',
  query: '(debugger_statement) @stmt',
  card: { message: 'x', remediation: 'y', examples: { bad: 'a', good: 'b' } },
  check() {},
  reduce() { throw new Error('reduce exploded'); },
});
";
        let project = Project::new(
            "reduce-throws",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
            ],
        );

        let error = project.run().expect_err("aborts");
        let rendered = error.to_string();
        assert!(rendered.contains("reduce exploded"), "{rendered}");
        assert!(
            rendered.contains("local/throws-in-reduce"),
            "the error should name the rule: {rendered}"
        );
    }

    #[test]
    fn facts_reach_reduce_in_the_same_order_on_every_run() {
        // The determinism invariant at the level a rule can observe: `ctx.facts()` is in
        // (file, sequence) order, so a rule that takes the first match — or builds a
        // "first seen wins" map — gives the same answer every run.
        //
        // This asserts the property, not the mechanism. Two things currently produce it,
        // rayon's order-preserving `collect` and the explicit sort, so removing either one
        // alone leaves this passing. The sort's own coverage is in `lanekeep_core::fact`,
        // where shuffled input makes its absence visible.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/first-fact-wins',
  query: '(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)))',
  card: { message: 'first', remediation: 'x', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) { ctx.emitFact({ kind: 'sym', symbol: ctx.text(m.name) }); },
  reduce(ctx) {
    const all = ctx.facts('sym');
    ctx.report({ file: 'order.ts', line: 1, column: 1 }, all.map((f) => `${f.file}:${f.symbol}`).join(','));
  },
});
";
        let files: Vec<(String, String)> = (0..12)
            .map(|i| {
                (
                    format!("src/f{i:02}.ts"),
                    format!("export const s{i:02} = {i};\n"),
                )
            })
            .collect();

        let mut layout: Vec<(&str, &str)> = vec![("rule.ts", RULE)];
        let config_source = config("");
        layout.push(("lanekeep.config.ts", &config_source));
        for (path, contents) in &files {
            layout.push((path, contents));
        }

        let project = Project::new("reduce-determinism", &layout);

        let first = project.run().expect("runs").violations[0].message.clone();
        for attempt in 0..4 {
            let again = project.run().expect("runs").violations[0].message.clone();
            assert_eq!(again, first, "fact order changed on attempt {attempt}");
        }

        // And it is the canonical order, not merely a repeatable one.
        assert!(
            first.starts_with("src/f00.ts:s00,src/f01.ts:s01,"),
            "facts are not in (file, sequence) order: {first}"
        );
    }

    #[test]
    fn a_rule_cannot_misattribute_a_fact_to_another_file() {
        // The host sets `file`, last, so a rule's own `file` key loses to it.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/lying-fact',
  query: '(export_statement) @stmt',
  card: { message: 'x', remediation: 'y', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) { ctx.emitFact({ kind: 'e', file: 'somewhere-else.ts' }); },
  reduce(ctx) {
    for (const f of ctx.facts('e')) ctx.report({ file: f.file, line: 1, column: 1 });
  },
});
";
        let project = Project::new(
            "reduce-misattribution",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert_eq!(outcome.violations[0].location.file.as_str(), "src/a.ts");
    }

    // --- tracked reads -------------------------------------------------------------------

    /// A rule that reads a sibling file and reports when it says so.
    const READING_RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/reads-config',
  query: '(export_statement) @stmt',
  card: {
    message: 'config says no',
    remediation: 'change the config, or the code',
    examples: { bad: 'export const a = 1;', good: 'const a = 1;' },
  },
  check(ctx, m) {
    const raw = ctx.readFile('policy.json');
    if (raw && JSON.parse(raw).forbidExports) ctx.report(m.stmt);
  },
});
";

    #[test]
    fn a_rule_can_read_another_file() {
        let project = Project::new(
            "reads-allowed",
            &[
                ("rule.ts", READING_RULE),
                ("lanekeep.config.ts", &config("")),
                ("policy.json", r#"{"forbidExports":true}"#),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );
        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", outcome.violations);
    }

    #[test]
    fn what_the_file_says_changes_the_result() {
        // Otherwise the test above would pass on a `readFile` that returned nothing.
        let project = Project::new(
            "reads-content",
            &[
                ("rule.ts", READING_RULE),
                ("lanekeep.config.ts", &config("")),
                ("policy.json", r#"{"forbidExports":false}"#),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
    }

    #[test]
    fn a_read_is_recorded_against_the_file_that_made_it() {
        // The shape a cache entry needs. A dependency recorded against the run, or leaked
        // from the previous file on the same worker, would invalidate the wrong entries.
        //
        // Enough files that workers necessarily handle several each: with only two, rayon
        // puts them on separate workers with separate `FileAccess`, and a missing reset
        // between files cannot show. `FileAccess::clear` is covered deterministically by
        // its own unit test; this covers the engine actually calling it.
        let mut layout: Vec<(String, String)> = vec![
            ("rule.ts".to_owned(), READING_RULE.to_owned()),
            ("lanekeep.config.ts".to_owned(), config("")),
            (
                "policy.json".to_owned(),
                r#"{"forbidExports":false}"#.to_owned(),
            ),
        ];
        // Odd files export and therefore read; even files do neither.
        for i in 0..24 {
            let body = if i % 2 == 0 {
                format!("const v{i} = {i};\n")
            } else {
                format!("export const v{i} = {i};\n")
            };
            layout.push((format!("src/f{i:02}.ts"), body));
        }
        let borrowed: Vec<(&str, &str)> = layout
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();

        let project = Project::new("reads-attributed", &borrowed);
        let outcome = project.run().expect("runs");

        for i in 0..24 {
            let file = FilePath::new(format!("src/f{i:02}.ts"));
            let deps = outcome.dependencies.get(&file);
            if i % 2 == 0 {
                assert!(
                    deps.is_none(),
                    "src/f{i:02}.ts read nothing but has {deps:?}"
                );
            } else {
                let deps = deps.unwrap_or_else(|| panic!("src/f{i:02}.ts should have read"));
                assert_eq!(deps.len(), 1);
                assert_eq!(deps[0].path.as_str(), "policy.json");
                assert!(deps[0].hash.is_some());
            }
        }
    }

    #[test]
    fn a_missing_file_is_recorded_as_a_dependency_too() {
        // The case that makes a cache wrong rather than cold: the answer "not there" has to
        // be invalidated when the file appears.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/wants-config',
  query: '(export_statement) @stmt',
  card: { message: 'no config', remediation: 'add one', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) {
    if (!ctx.fileExists('tsconfig.json')) ctx.report(m.stmt);
  },
});
";
        let project = Project::new(
            "reads-absent",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);

        let deps = outcome
            .dependencies
            .get(&FilePath::new("src/a.ts"))
            .expect("the miss is a dependency");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].path.as_str(), "tsconfig.json");
        assert_eq!(deps[0].hash, None, "absence is recorded as absence");
    }

    #[test]
    fn reading_outside_the_project_aborts_the_run() {
        // Not a rule that reports nothing: a rule reaching outside the project is a rule
        // doing something it must never do, and a run that continued would be reporting a
        // result produced by code that tried.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/escapes',
  query: '(export_statement) @stmt',
  card: { message: 'x', remediation: 'y', examples: { bad: 'a', good: 'b' } },
  check(ctx) { ctx.readFile('../../../etc/passwd'); },
});
";
        let project = Project::new(
            "reads-escape",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        let error = project.run().expect_err("aborts");
        let rendered = error.to_string();
        assert!(rendered.contains("outside the project root"), "{rendered}");
        assert!(rendered.contains("local/escapes"), "{rendered}");
    }

    #[test]
    fn reading_the_same_file_from_two_files_records_it_under_both() {
        let project = Project::new(
            "reads-shared",
            &[
                ("rule.ts", READING_RULE),
                ("lanekeep.config.ts", &config("")),
                ("policy.json", r#"{"forbidExports":false}"#),
                ("src/a.ts", "export const a = 1;\n"),
                ("src/b.ts", "export const b = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        for file in ["src/a.ts", "src/b.ts"] {
            let deps = outcome
                .dependencies
                .get(&FilePath::new(file))
                .unwrap_or_else(|| panic!("{file} should depend on the policy"));
            assert_eq!(deps[0].path.as_str(), "policy.json");
        }

        // The same bytes, so the same hash — a cache must not see two different
        // dependencies on one file.
        let a = &outcome.dependencies[&FilePath::new("src/a.ts")][0];
        let b = &outcome.dependencies[&FilePath::new("src/b.ts")][0];
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn dependencies_are_the_same_on_every_run() {
        let project = Project::new(
            "reads-deterministic",
            &[
                ("rule.ts", READING_RULE),
                ("lanekeep.config.ts", &config("")),
                ("policy.json", r#"{"forbidExports":false}"#),
                ("src/a.ts", "export const a = 1;\n"),
                ("src/b.ts", "export const b = 1;\n"),
                ("src/c.ts", "export const c = 1;\n"),
            ],
        );
        let first = project.run().expect("runs").dependencies;
        assert!(!first.is_empty());
        for attempt in 0..4 {
            assert_eq!(
                project.run().expect("runs").dependencies,
                first,
                "dependencies changed on attempt {attempt}"
            );
        }
    }

    #[test]
    fn the_read_surface_is_absent_from_the_reduce_phase() {
        // Reduce reads would be run-level dependencies, not per-file ones, and storing them
        // in a per-file entry would attribute them to whichever file came last. Until the
        // cache can express that, the functions are not there to be misused.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/reduce-reads',
  query: '(export_statement) @stmt',
  card: { message: 'x', remediation: 'y', examples: { bad: 'a', good: 'b' } },
  check() {},
  reduce(ctx) {
    const absent = ctx.readFile === undefined && ctx.fileExists === undefined;
    ctx.report({ file: 'probe.ts', line: absent ? 1 : 2, column: 1 });
  },
});
";
        let project = Project::new(
            "reads-reduce",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert_eq!(
            outcome.violations[0].location.position.line, 1,
            "reads must not be reachable from a reduce phase"
        );
    }

    // --- the cache -----------------------------------------------------------------------

    impl Project {
        /// Run with the cache disabled, for comparing against a warm run.
        fn run_cold(&self) -> Result<Outcome, RunError> {
            self.build().map(Engine::without_cache)?.run()
        }

        /// The engine, without running it.
        fn build(&self) -> Result<Engine, RunError> {
            let root = RuleRoot::new(&self.dir).expect("canonicalizes");
            let config_path = self.dir.join("lanekeep.config.ts");
            let sandbox =
                lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                    .expect("sandbox");
            let config = lanekeep_config::load(&sandbox, &root, &config_path)
                .unwrap_or_else(|e| panic!("config failed to load: {e}"));
            Engine::prepare(
                &config,
                &self.dir,
                root,
                &config_path,
                &lanekeep_lang_js::registry(),
                Arc::new(TypeScript),
                Arc::new(JavaScript),
            )
        }

        fn cache(&self) -> Store {
            Store::load(&self.dir)
        }
    }

    fn rendered(outcome: &Outcome) -> Vec<String> {
        outcome
            .violations
            .iter()
            .map(|v| {
                format!(
                    "{}:{}:{} {} {}",
                    v.location.file.as_str(),
                    v.location.position.line,
                    v.location.position.column,
                    v.rule_id,
                    v.message
                )
            })
            .collect()
    }

    #[test]
    fn a_warm_run_agrees_with_a_cold_one() {
        let project = Project::new(
            "cache-agrees",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\nconst a = 1;\n"),
                ("src/b.ts", "const b = 1;\ndebugger;\n"),
                ("src/c.ts", "const c = 1;\n"),
            ],
        );

        let cold = rendered(&project.run().expect("runs"));
        let warm = rendered(&project.run().expect("runs"));
        assert_eq!(warm, cold, "the cache changed the answer");
        assert!(!cold.is_empty(), "the fixture should report something");
    }

    #[test]
    fn a_run_writes_a_cache() {
        let project = Project::new(
            "cache-written",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        assert!(project.cache().is_empty(), "nothing before the first run");
        project.run().expect("runs");
        assert!(!project.cache().is_empty(), "the run stored nothing");
    }

    #[test]
    fn a_cached_result_is_actually_used() {
        // Agreeing with a cold run proves nothing on its own — a cache that was never read
        // would agree too. So doctor the stored entry and show the doctored value comes
        // back: that can only happen through the cache.
        let project = Project::new(
            "cache-used",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());

        let store = project.cache();
        let key = *store
            .keys()
            .next()
            .expect("the run stored an entry for the file");

        let mut doctored = Store::empty();
        doctored.insert(
            key,
            lanekeep_cache::Entry {
                violations: vec![Violation {
                    rule_id: "local/no-debugger".parse().expect("valid id"),
                    location: Location::new(FilePath::new("src/a.ts"), Position::new(7, 3)),
                    message: "from the cache".to_owned(),
                    remediation: "nothing".to_owned(),
                    severity: Severity::Error,
                    fix: None,
                }],
                facts: Vec::new(),
                dependencies: Vec::new(),
                suppressions: Vec::new(),
                used_suppressions: Vec::new(),
            },
        );
        doctored.save(&project.dir);

        let outcome = project.run().expect("runs");
        assert_eq!(
            rendered(&outcome),
            vec!["src/a.ts:7:3 local/no-debugger from the cache"],
            "the cached entry was not used"
        );
    }

    #[test]
    fn editing_a_file_invalidates_it() {
        let project = Project::new(
            "cache-edited",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const a = 1;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());

        project.write("src/a.ts", "debugger;\n");
        assert_eq!(
            project.run().expect("runs").violations.len(),
            1,
            "an edited file kept its stale result"
        );
    }

    #[test]
    fn moving_a_file_invalidates_it() {
        // Path gates make results path-sensitive, so identical bytes at a new path are not
        // a hit. This fixture's rule has no path gate, but the key must not depend on that.
        let project = Project::new(
            "cache-moved",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        project.run().expect("runs");

        fs::remove_file(project.dir.join("src/a.ts")).expect("removes");
        project.write("src/moved.ts", "debugger;\n");

        let outcome = project.run().expect("runs");
        assert_eq!(
            outcome.violations[0].location.file.as_str(),
            "src/moved.ts",
            "the violation followed the old path"
        );
    }

    #[test]
    fn editing_a_tracked_dependency_invalidates_the_files_that_read_it() {
        // The reason tracked effects exist. Nothing about `src/a.ts` changed, and its result
        // still has to be recomputed.
        let project = Project::new(
            "cache-dependency",
            &[
                ("rule.ts", READING_RULE),
                ("lanekeep.config.ts", &config("")),
                ("policy.json", r#"{"forbidExports":false}"#),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());

        project.write("policy.json", r#"{"forbidExports":true}"#);
        assert_eq!(
            project.run().expect("runs").violations.len(),
            1,
            "a changed dependency did not invalidate"
        );
    }

    #[test]
    fn a_dependency_that_appears_invalidates() {
        // The case a cache is wrong rather than merely cold without: a rule was told a file
        // was absent, and creating it has to reopen the question.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/wants-config',
  query: '(export_statement) @stmt',
  card: { message: 'no config', remediation: 'add one', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) {
    if (!ctx.fileExists('tsconfig.json')) ctx.report(m.stmt);
  },
});
";
        let project = Project::new(
            "cache-appeared",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );
        assert_eq!(project.run().expect("runs").violations.len(), 1);

        project.write("tsconfig.json", "{}");
        assert!(
            project.run().expect("runs").violations.is_empty(),
            "a dependency that appeared did not invalidate"
        );
    }

    #[test]
    fn changing_the_ruleset_invalidates_everything() {
        let project = Project::new(
            "cache-ruleset",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        assert_eq!(project.run().expect("runs").violations.len(), 1);

        // Same file, different rule: it now reports nothing.
        project.write(
            "rule.ts",
            &DEBUGGER_RULE.replace("ctx.report(m.stmt);", "/* nothing */"),
        );
        assert!(
            project.run().expect("runs").violations.is_empty(),
            "an edited rule kept its stale results"
        );
    }

    #[test]
    fn changing_the_config_invalidates_everything() {
        let project = Project::new(
            "cache-config",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        assert_eq!(project.run().expect("runs").violations.len(), 1);

        project.write(
            "lanekeep.config.ts",
            &config(", severity: { 'local/no-debugger': 'off' }"),
        );
        assert!(
            project.run().expect("runs").violations.is_empty(),
            "a config change did not invalidate"
        );
    }

    #[test]
    fn a_corrupt_cache_still_produces_the_right_answer() {
        // Disposability, end to end: garbage on disk costs a recompute and nothing else.
        let project = Project::new(
            "cache-corrupt",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        let expected = rendered(&project.run().expect("runs"));

        let path = Store::path_for(&project.dir);
        fs::write(&path, b"\x00\x01\x02 not a cache").expect("writes");

        assert_eq!(rendered(&project.run().expect("runs")), expected);
    }

    #[test]
    fn caching_can_be_turned_off() {
        let project = Project::new(
            "cache-off",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );
        let outcome = project.run_cold().expect("runs");
        assert_eq!(outcome.violations.len(), 1);
        assert!(
            project.cache().is_empty(),
            "a run with caching off wrote a cache"
        );
    }

    #[test]
    fn facts_survive_a_warm_run() {
        // The reduce phase runs every time, over facts that may all have come from the
        // cache. A cache that dropped them would make cross-file rules go quiet on the
        // second run — reporting on a cold run and nothing on a warm one is the worst
        // possible failure, because it looks like the problem was fixed.
        let project = Project::new(
            "cache-facts",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "export function used() {}\nexport function spare() {}\n",
                ),
                ("src/b.ts", "import { used } from './a';\nused();\n"),
            ],
        );

        let cold = rendered(&project.run().expect("runs"));
        assert_eq!(cold.len(), 1, "{cold:?}");
        assert_eq!(rendered(&project.run().expect("runs")), cold);
        assert_eq!(rendered(&project.run().expect("runs")), cold);
    }

    #[test]
    fn a_cache_file_does_not_churn() {
        // Byte-identical across runs over unchanged input. A file that rewrote itself every
        // run would be a spurious diff for anyone who commits it.
        let project = Project::new(
            "cache-stable",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
                ("src/b.ts", "const b = 1;\n"),
            ],
        );
        project.run().expect("runs");
        let first = fs::read(Store::path_for(&project.dir)).expect("reads");
        project.run().expect("runs");
        let second = fs::read(Store::path_for(&project.dir)).expect("reads");
        assert_eq!(first, second, "the cache file churned");
    }

    #[test]
    fn entries_for_deleted_files_do_not_accumulate() {
        let project = Project::new(
            "cache-prune",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
                ("src/b.ts", "debugger;\n"),
            ],
        );
        project.run().expect("runs");
        assert_eq!(project.cache().len(), 2);

        fs::remove_file(project.dir.join("src/b.ts")).expect("removes");
        project.run().expect("runs");
        assert_eq!(
            project.cache().len(),
            1,
            "an entry outlived the file it was for"
        );
    }

    #[test]
    fn a_partial_run_does_not_discard_other_files_entries() {
        // `--staged` saving only what it processed would wipe the cache for every file it
        // never looked at, leaving the next full run cold — the opposite of what an
        // incremental entry point is for.
        let project = Project::new(
            "cache-partial",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
                ("src/b.ts", "const b = 1;\n"),
                ("src/c.ts", "const c = 1;\n"),
            ],
        );
        project.run().expect("runs");
        assert_eq!(project.cache().len(), 3);

        let engine = project.build().expect("prepares");
        engine
            .run_over(&[FilePath::new("src/a.ts")])
            .expect("runs over one file");

        assert_eq!(
            project.cache().len(),
            3,
            "a partial run discarded entries for files it did not look at"
        );
    }

    #[test]
    fn a_full_run_still_prunes() {
        // The other half: pruning has to keep working, or entries for deleted files
        // accumulate forever.
        let project = Project::new(
            "cache-prune-still",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
                ("src/b.ts", "const b = 1;\n"),
            ],
        );
        project.run().expect("runs");
        assert_eq!(project.cache().len(), 2);

        fs::remove_file(project.dir.join("src/b.ts")).expect("removes");
        project.run().expect("runs");
        assert_eq!(project.cache().len(), 1);
    }

    // --- suppressions ----------------------------------------------------------------------

    impl Project {
        /// Run with a fixed date, so an expiry can be asserted without waiting for one.
        fn run_on(&self, today: &str) -> Result<Outcome, RunError> {
            let date = Date::parse(today).expect("valid date");
            self.build().map(|engine| engine.with_today(date))?.run()
        }
    }

    fn messages(outcome: &Outcome) -> Vec<&str> {
        outcome
            .violations
            .iter()
            .map(|v| v.message.as_str())
            .collect()
    }

    #[test]
    fn a_next_line_directive_silences_the_line_below_it() {
        let project = Project::new(
            "suppress-next-line",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: legacy entry point\n\
                     debugger;\n",
                ),
            ],
        );
        assert!(
            project.run().expect("runs").violations.is_empty(),
            "the directive did not silence the violation"
        );
    }

    #[test]
    fn a_directive_silences_only_the_line_it_names() {
        let project = Project::new(
            "suppress-scope",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: legacy\n\
                     debugger;\n\
                     debugger;\n",
                ),
            ],
        );
        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert_eq!(outcome.violations[0].location.position.line, 3);
    }

    #[test]
    fn a_file_directive_silences_every_line() {
        let project = Project::new(
            "suppress-file",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-file local/no-debugger reason: generated fixture\n\
                     debugger;\n\
                     debugger;\n",
                ),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
    }

    #[test]
    fn a_directive_naming_another_rule_silences_nothing() {
        let project = Project::new(
            "suppress-other-rule",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/something-else reason: unrelated\n\
                     debugger;\n",
                ),
            ],
        );
        assert_eq!(project.run().expect("runs").violations.len(), 1);
    }

    #[test]
    fn a_malformed_directive_is_reported() {
        // The failure this exists to prevent: a directive that looks like it works, does
        // not, and says nothing. Both the missing reason and the violation it failed to
        // suppress have to surface.
        let project = Project::new(
            "suppress-malformed",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger\ndebugger;\n",
                ),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 2, "{:?}", messages(&outcome));
        assert!(
            messages(&outcome)
                .iter()
                .any(|m| m.contains("no `reason:`")),
            "{:?}",
            messages(&outcome)
        );
        assert!(
            outcome
                .violations
                .iter()
                .any(|v| v.rule_id.to_string() == "lanekeep/suppression"),
            "reported under the wrong id"
        );
    }

    #[test]
    fn an_expired_directive_is_reported_and_still_silences() {
        // It expired, which is worth saying — but suddenly reporting everything it covered
        // would turn a deadline into an avalanche on the day it passed.
        let project = Project::new(
            "suppress-expired",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: pending rewrite expires: 2026-01-01\n\
                     debugger;\n",
                ),
            ],
        );

        let outcome = project.run_on("2026-08-01").expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert!(
            outcome.violations[0]
                .message
                .contains("expired on 2026-01-01"),
            "{:?}",
            messages(&outcome)
        );
        assert!(
            outcome.violations[0].message.contains("pending rewrite"),
            "the reason should be quoted back: {:?}",
            messages(&outcome)
        );
    }

    #[test]
    fn a_directive_that_has_not_expired_is_quiet() {
        let project = Project::new(
            "suppress-unexpired",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: pending expires: 2026-12-31\n\
                     debugger;\n",
                ),
            ],
        );
        assert!(
            project
                .run_on("2026-08-01")
                .expect("runs")
                .violations
                .is_empty()
        );
    }

    #[test]
    fn a_directive_expires_the_day_after_its_date() {
        // On the date itself it still holds: an expiry is a deadline, and a deadline of the
        // 31st is not missed on the 31st.
        let project = Project::new(
            "suppress-boundary",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-file local/no-debugger reason: x expires: 2026-08-01\n\
                     debugger;\n",
                ),
            ],
        );
        assert!(
            project
                .run_on("2026-08-01")
                .expect("runs")
                .violations
                .is_empty()
        );
        assert_eq!(
            project.run_on("2026-08-02").expect("runs").violations.len(),
            1
        );
    }

    #[test]
    fn an_expiring_directive_is_not_served_stale_from_the_cache() {
        // The cache-soundness case. A file cached the day before expiry must not keep its
        // suppressed result the day after — an expiry that a warm run ignored would never
        // expire at all, which is the one thing an expiry exists to prevent.
        let project = Project::new(
            "suppress-cache-date",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-file local/no-debugger reason: x expires: 2026-08-01\n\
                     debugger;\n",
                ),
            ],
        );

        assert!(
            project
                .run_on("2026-08-01")
                .expect("runs")
                .violations
                .is_empty()
        );
        let after = project.run_on("2026-08-02").expect("runs");
        assert_eq!(
            after.violations.len(),
            1,
            "a warm run served an expired suppression: {:?}",
            messages(&after)
        );
    }

    #[test]
    fn suppressions_survive_a_warm_run() {
        let project = Project::new(
            "suppress-warm",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-file local/no-debugger reason: generated\ndebugger;\n",
                ),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
        assert!(
            project.run().expect("runs").violations.is_empty(),
            "the warm run reported what the cold one suppressed"
        );
    }

    #[test]
    fn a_cross_file_violation_is_silenced_by_the_directive_where_it_lands() {
        // A reduce-phase violation is reported at the site a fact came from, in a file the
        // rule was never "checking" — and possibly one that was a cache hit. The directives
        // that matter are that file's.
        let project = Project::new(
            "suppress-cross-file",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "export function used() {}\n\
                     // lanekeep-ignore-next-line local/no-unused-exports reason: public API\n\
                     export function spare() {}\n",
                ),
                ("src/b.ts", "import { used } from './a';\nused();\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert!(
            outcome.violations.is_empty(),
            "a cross-file violation ignored the directive at its site: {:?}",
            messages(&outcome)
        );
    }

    #[test]
    fn a_cross_file_violation_survives_a_directive_for_another_rule() {
        let project = Project::new(
            "suppress-cross-file-other",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "export function used() {}\n\
                     // lanekeep-ignore-next-line local/unrelated reason: x\n\
                     export function spare() {}\n",
                ),
                ("src/b.ts", "import { used } from './a';\nused();\n"),
            ],
        );
        assert_eq!(project.run().expect("runs").violations.len(), 1);
    }

    // --- unused suppressions ---------------------------------------------------------------

    impl Project {
        fn run_reporting_unused(&self) -> Result<Outcome, RunError> {
            self.build()
                .map(Engine::reporting_unused_suppressions)?
                .run()
        }
    }

    #[test]
    fn a_suppression_that_silenced_nothing_is_reported() {
        let project = Project::new(
            "unused-reported",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: was needed once\n\
                     const a = 1;\n",
                ),
            ],
        );

        let outcome = project.run_reporting_unused().expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert!(
            outcome.violations[0].message.contains("silenced nothing"),
            "{:?}",
            messages(&outcome)
        );
        assert!(
            outcome.violations[0].message.contains("was needed once"),
            "the reason should be quoted back: {:?}",
            messages(&outcome)
        );
    }

    #[test]
    fn a_suppression_that_did_its_job_is_not_reported() {
        let project = Project::new(
            "unused-used",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: legacy\ndebugger;\n",
                ),
            ],
        );
        assert!(
            project
                .run_reporting_unused()
                .expect("runs")
                .violations
                .is_empty()
        );
    }

    #[test]
    fn unused_suppressions_are_quiet_without_the_flag() {
        // Hygiene, on request. It must not appear in everyone's inner loop.
        let project = Project::new(
            "unused-off",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: stale\nconst a = 1;\n",
                ),
            ],
        );
        assert!(project.run().expect("runs").violations.is_empty());
    }

    #[test]
    fn an_unused_suppression_is_a_warning_not_an_error() {
        // Turning on a hygiene report must not fail a build that was passing.
        let project = Project::new(
            "unused-severity",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: stale\nconst a = 1;\n",
                ),
            ],
        );
        let outcome = project.run_reporting_unused().expect("runs");
        assert_eq!(outcome.violations[0].severity, Severity::Warn);
        assert!(!lanekeep_core::any_failing(&outcome.violations));
    }

    #[test]
    fn usage_survives_a_warm_run() {
        // The case this needed a cache field for: a warm run sees the survivors and not what
        // was hidden, so without the recorded usage every suppression in a cached file would
        // suddenly look unused.
        let project = Project::new(
            "unused-warm",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger reason: legacy\ndebugger;\n",
                ),
            ],
        );

        assert!(
            project
                .run_reporting_unused()
                .expect("runs")
                .violations
                .is_empty()
        );
        let warm = project.run_reporting_unused().expect("runs");
        assert!(
            warm.violations.is_empty(),
            "a warm run called a used suppression unused: {:?}",
            messages(&warm)
        );
    }

    #[test]
    fn a_suppression_used_only_by_a_cross_file_rule_is_not_unused() {
        // A directive can be the only thing standing between a reduce-phase violation and
        // the report. Counting usage only during the per-file pass would call it unused.
        let project = Project::new(
            "unused-cross-file",
            &[
                ("rule.ts", UNUSED_EXPORTS_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "export function used() {}\n\
                     // lanekeep-ignore-next-line local/no-unused-exports reason: public API\n\
                     export function spare() {}\n",
                ),
                ("src/b.ts", "import { used } from './a';\nused();\n"),
            ],
        );

        let outcome = project.run_reporting_unused().expect("runs");
        assert!(
            outcome.violations.is_empty(),
            "a directive used by a cross-file rule was called unused: {:?}",
            messages(&outcome)
        );
    }

    #[test]
    fn a_malformed_directive_is_not_also_reported_as_unused() {
        // It already has a violation saying what is wrong with it. A second one saying it
        // silenced nothing would be true, unhelpful, and confusing.
        let project = Project::new(
            "unused-malformed",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                (
                    "src/a.ts",
                    "// lanekeep-ignore-next-line local/no-debugger\nconst a = 1;\n",
                ),
            ],
        );

        let outcome = project.run_reporting_unused().expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert!(
            outcome.violations[0].message.contains("no `reason:`"),
            "{:?}",
            messages(&outcome)
        );
    }

    // --- ctx.today and the cache -----------------------------------------------------------

    /// A rule that reports only when the date it is given starts with a given year.
    const DATE_RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/dated',
  query: '(export_statement) @stmt',
  card: { message: 'dated', remediation: 'x', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) {
    if (ctx.today.startsWith('2027')) ctx.report(m.stmt, `it is ${ctx.today}`);
  },
});
";

    #[test]
    fn a_rule_can_read_the_date() {
        let project = Project::new(
            "today-read",
            &[
                ("rule.ts", DATE_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );
        let outcome = project.run_on("2027-03-04").expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert!(outcome.violations[0].message.contains("2027-03-04"));
    }

    #[test]
    fn a_result_that_read_the_date_is_not_served_across_days() {
        // The cache-soundness case for `ctx.today`. Without tracking the read, the answer
        // computed in 2026 would be served in 2027 forever — a date comparison frozen at
        // whenever the cache happened to be written.
        let project = Project::new(
            "today-cache",
            &[
                ("rule.ts", DATE_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        assert!(
            project
                .run_on("2026-12-31")
                .expect("runs")
                .violations
                .is_empty()
        );
        let later = project.run_on("2027-01-01").expect("runs");
        assert_eq!(
            later.violations.len(),
            1,
            "a warm run served a date-dependent result from another day: {:?}",
            messages(&later)
        );
    }

    #[test]
    fn a_result_that_ignored_the_date_survives_across_days() {
        // The other half, and the reason the read is tracked rather than assumed: dating
        // every entry would re-key the whole corpus daily.
        //
        // Asserted on the stored *bytes*, not the entry count. A re-keyed entry replaces the
        // one it supersedes, so the count is identical either way — it was the count I
        // reached for first, and it proved nothing.
        let project = Project::new(
            "today-undated",
            &[
                ("rule.ts", DEBUGGER_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "debugger;\n"),
            ],
        );

        project.run_on("2026-12-31").expect("runs");
        let before = fs::read(Store::path_for(&project.dir)).expect("reads");

        let outcome = project.run_on("2027-01-01").expect("runs");
        assert_eq!(outcome.violations.len(), 1);

        let after = fs::read(Store::path_for(&project.dir)).expect("reads");
        assert_eq!(
            before, after,
            "a result that never read the date was re-keyed across days"
        );
    }

    #[test]
    fn a_result_that_read_the_date_is_re_keyed_across_days() {
        // The converse, on the same evidence. Together these pin both directions: dateless
        // entries keep their key, dated ones do not.
        let project = Project::new(
            "today-dated-key",
            &[
                ("rule.ts", DATE_RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "export const a = 1;\n"),
            ],
        );

        project.run_on("2026-12-31").expect("runs");
        let before = fs::read(Store::path_for(&project.dir)).expect("reads");

        project.run_on("2027-01-01").expect("runs");
        let after = fs::read(Store::path_for(&project.dir)).expect("reads");
        assert_ne!(
            before, after,
            "a result that read the date kept its key across days"
        );
    }

    #[test]
    fn loc_reaches_a_reduce_phase_through_a_fact() {
        // The shape `ctx.loc` exists for: emit it on a fact, report at it later, no glue.
        const RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/loc-through-facts',
  query: '(export_statement) @stmt',
  card: { message: 'via loc', remediation: 'x', examples: { bad: 'a', good: 'b' } },
  check(ctx, m) { ctx.emitFact({ kind: 'site', at: ctx.loc(m.stmt) }); },
  reduce(ctx) {
    for (const f of ctx.facts('site')) ctx.report(f.at, 'reported at a remembered place');
  },
});
";
        let project = Project::new(
            "loc-facts",
            &[
                ("rule.ts", RULE),
                ("lanekeep.config.ts", &config("")),
                ("src/a.ts", "const x = 1;\nexport const a = 1;\n"),
            ],
        );

        let outcome = project.run().expect("runs");
        assert_eq!(outcome.violations.len(), 1, "{:?}", messages(&outcome));
        assert_eq!(outcome.violations[0].location.file.as_str(), "src/a.ts");
        assert_eq!(outcome.violations[0].location.position.line, 2);
    }

    /// The run's own wall-clock budget, which is spent mostly outside any sandbox.
    ///
    /// `AGENTS.md` recorded the gap these cover: the budget was polled by QuickJS's interrupt
    /// handler and by nothing else, so it only bounded a run *while JavaScript was executing*.
    /// Four hundred files against a one-line rule ran to completion under a one-millisecond
    /// budget, because §15's cold cost is dominated by Rust-side reading, parsing and query
    /// matching and none of that is a place the handler runs.
    ///
    /// # Why these fixtures are cheap on purpose, when every other budget test is expensive
    ///
    /// The rest of this repository's limit tests need a rule that burns real bytecode, or they
    /// pass because the work was fast rather than because a limit was enforced. Here the
    /// requirement is the exact opposite and for the same reason: the handler has to be so
    /// cheap that the *only* thing that can stop the run is the check outside it. A rule doing
    /// real work would be stopped by the interrupt handler, and the test would pass against
    /// the bug.
    ///
    /// Measured against the commit before this one, this corpus ran to completion: 400 files
    /// and 400 `check` invocations in 84 ms under a 1 ms budget, with nothing ever asked to
    /// stop. That measurement is what says these cases are not passing for a reason unrelated
    /// to the check they are about.
    mod run_budget {
        use super::*;

        /// Enough files that a millisecond cannot cover them.
        ///
        /// The number from the `AGENTS.md` trap, and the margin is wide rather than tuned: the
        /// corpus takes ~84 ms in a debug build, so the budget below is breached roughly eighty
        /// times over.
        const FILES: usize = 400;

        /// A file the rule matches, so a handler really is invoked once per file.
        const MATCHED: &str = "export function a() {\n  debugger;\n}\n";

        /// `FILES` identical files under one global budget, with `no-debugger` over them.
        fn corpus(name: &str, global_ms: u64) -> Project {
            let config = config(&format!(", timeouts: {{ global: {global_ms} }}"));
            let mut owned: Vec<(String, String)> = vec![
                ("rule.ts".to_owned(), DEBUGGER_RULE.to_owned()),
                ("lanekeep.config.ts".to_owned(), config),
            ];
            for i in 0..FILES {
                owned.push((format!("src/f{i}.ts"), MATCHED.to_owned()));
            }
            let borrowed: Vec<(&str, &str)> = owned
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            Project::new(name, &borrowed)
        }

        #[test]
        fn a_corpus_of_cheap_invocations_is_stopped_by_the_runs_budget() {
            let project = corpus("run-budget-lowered", 1);

            let error = project
                .run_cold()
                .expect_err("a run whose budget is spent must not finish the corpus");

            // The variant, not the wording, and that is what makes this discriminating. A
            // breach the interrupt handler noticed arrives as `RunError::Rule`, naming a rule
            // and a file; this one is the walker's own, and it names neither because neither
            // is at fault.
            assert!(
                matches!(error, RunError::RunTimeout { .. }),
                "the run had to be stopped between files rather than inside a handler: {error}"
            );
        }

        #[test]
        fn the_same_corpus_completes_when_the_budget_is_raised() {
            // `AGENTS.md`: a test that only *lowers* a limit passes against a limit that is
            // read and then dropped, because the run completes either way. This is the half
            // that discriminates — and it is also the control for the case above, since
            // without it "the run was stopped" is equally consistent with a corpus that can
            // no longer be checked at all.
            let project = corpus("run-budget-raised", 60_000);

            let outcome = project
                .run_cold()
                .expect("a minute is ample for four hundred one-line files");
            assert_eq!(
                outcome.violations.len(),
                FILES,
                "every file has to have been checked, or the case above stopped nothing"
            );
        }

        /// A rule that throws on the one file whose text says `boom`, and nowhere else.
        const SELECTIVE_RULE: &str = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/selective',\n\
              query: '(identifier) @id',\n\
              card: { message: 'x', remediation: 'y', examples: { bad: 'a', good: 'b' } },\n\
              check(ctx, m) { if (ctx.text(m.id) === 'boom') throw new Error('kaboom'); },\n\
            });\n";

        #[test]
        fn an_aborted_run_still_commits_the_files_that_finished() {
            // Architecture §6.8: cache entries for files that fully completed are still
            // committed, because otherwise a corpus that dies on a cold run dies identically
            // on every retry and there is no way to make progress. That mattered little while
            // the run budget went unenforced — the run simply finished. It is load-bearing the
            // moment the check above exists.
            //
            // The abort here is a thrown rule rather than a timeout, deliberately: which files
            // finish is then a property of the corpus rather than of how fast the machine is.
            const GOOD: usize = 40;
            let mut owned: Vec<(String, String)> = vec![
                ("rule.ts".to_owned(), SELECTIVE_RULE.to_owned()),
                ("lanekeep.config.ts".to_owned(), config("")),
            ];
            for i in 0..GOOD {
                owned.push((
                    format!("src/f{i}.ts"),
                    "export const fine = 1;\n".to_owned(),
                ));
            }
            owned.push((
                "src/zzz.ts".to_owned(),
                "export const boom = 1;\n".to_owned(),
            ));
            let borrowed: Vec<(&str, &str)> = owned
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            let project = Project::new("run-budget-partial-cache", &borrowed);

            project.run().expect_err("one file's rule throws");

            assert_eq!(
                Store::load(&project.dir).len(),
                GOOD,
                "every file that completed in full has to have an entry, and the one that did \
                 not must have none"
            );
        }

        #[test]
        fn an_aborted_run_does_not_prune_what_it_never_reached() {
            // The other half, and the one that would quietly destroy a cache rather than
            // merely fail to fill it. A run that saw the whole corpus may prune, because what
            // it produced no entry for no longer exists — that is what ages a deleted file
            // out. A run the budget stopped produced entries for a fraction of the corpus and
            // never looked at the rest, so saving only what it produced would age out every
            // file it never reached, and the next run would be *colder* than the one that
            // failed.
            let project = corpus("run-budget-no-prune", 60_000);
            project.run().expect("a minute is ample");
            assert_eq!(
                Store::load(&project.dir).len(),
                FILES,
                "the whole corpus is cached"
            );

            // The same corpus under a budget it cannot meet. Lowering it changes `config_hash`
            // — `timeouts.global` is a cache-key input — so this run is cold as well as short,
            // which is the worst case for the save: almost nothing of the corpus is fresh, and
            // everything that is already stored belongs to a key this run will never write.
            project.write("lanekeep.config.ts", &config(", timeouts: { global: 1 }"));
            let error = project.run().expect_err("one millisecond is not enough");
            assert!(matches!(error, RunError::RunTimeout { .. }), "{error}");

            assert!(
                Store::load(&project.dir).len() >= FILES,
                "an aborted run pruned entries for files it never reached"
            );
        }
    }

    /// The second dispatch path: rules whose handlers are a WebAssembly component.
    ///
    /// Every test above runs TypeScript rules through QuickJS and keeps doing so, which is what
    /// makes this module a check that a path was *added*. The two engines share one corpus, one
    /// clock, one read memo per file, and one sorted output.
    mod components {
        use super::*;

        /// The rule-shaped fixture, built by `just wasm-fixtures`.
        ///
        /// Referenced by path rather than `include_bytes!` because the engine's own loader is
        /// what is under test — it reads the file, precompiles it into the project's
        /// `.lanekeep/components`, and checks its import list before anything can instantiate.
        fn fixture() -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lanekeep-wasm/tests/fixtures/engine-rule.wasm")
        }

        /// The query the fixture is written against: it reports at `@target`.
        const QUERY: &str = "(variable_declarator name: (identifier) @target)";

        /// A `RuleSpec` backed by the fixture component.
        ///
        /// Built by hand, and that is not a shortcut around anything — `lanekeep-config` cannot
        /// produce one, because `wit/world.wit` has no `metadata` export and a component has
        /// therefore nowhere to put its own `id`, `query` or `card`. See [`RuleSpec::component`].
        /// What the engine dispatches on is this field, so a hand-built spec exercises exactly
        /// the production path.
        fn component_rule(id: &str, index: usize, has_reduce: bool) -> RuleSpec {
            RuleSpec {
                index,
                id: id.parse().expect("a well-formed rule id"),
                languages: vec!["typescript".to_owned()],
                severity: Severity::Error,
                card: lanekeep_core::RuleCard {
                    message: "a component rule fired".to_owned(),
                    remediation: "n/a".to_owned(),
                    examples: lanekeep_core::Examples {
                        bad: "const x = 1;".to_owned(),
                        good: "nothing".to_owned(),
                    },
                },
                query: QUERY.to_owned(),
                gates: lanekeep_core::Gates::default(),
                timeout: None,
                has_reduce,
                component: Some(fixture()),
            }
        }

        impl Project {
            /// Prepare an engine over this project's config plus some component-backed rules.
            ///
            /// Fallible variant, for the tests about a component that cannot be loaded.
            fn prepared(&self, extra: Vec<RuleSpec>) -> Result<Engine, RunError> {
                let root = RuleRoot::new(&self.dir).expect("canonicalizes");
                let config_path = self.dir.join("lanekeep.config.ts");
                let sandbox =
                    lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                        .expect("sandbox");
                let mut config = lanekeep_config::load(&sandbox, &root, &config_path)
                    .unwrap_or_else(|e| panic!("config failed to load: {e}"));
                config.rules.extend(extra);

                Engine::prepare(
                    &config,
                    &self.dir,
                    root,
                    &config_path,
                    &lanekeep_lang_js::registry(),
                    Arc::new(TypeScript),
                    Arc::new(JavaScript),
                )
            }

            /// The prepared engine, for a test reaching inside it.
            fn engine(&self, extra: Vec<RuleSpec>) -> Engine {
                self.prepared(extra).expect("prepares")
            }

            /// Load the project's config, add component-backed rules to it, and run cold.
            fn run_with(&self, extra: Vec<RuleSpec>) -> Result<Outcome, RunError> {
                self.prepared(extra)?.without_cache().run()
            }
        }

        /// A config declaring the `local` namespace and importing whichever rule modules it is
        /// given, in order.
        fn config_with(modules: &[&str]) -> String {
            let mut imports = String::new();
            for (i, m) in modules.iter().enumerate() {
                use std::fmt::Write as _;
                let _ = writeln!(imports, "import r{i} from '{m}';");
            }
            let names: Vec<String> = (0..modules.len()).map(|i| format!("r{i}")).collect();
            format!(
                "import {{ defineConfig }} from 'lanekeep';\n\
                 {imports}\
                 export default defineConfig({{ include: ['src/**/*.ts'], \
                 namespaces: ['local'], rules: [{}] }});\n",
                names.join(", ")
            )
        }

        /// A TypeScript rule reporting every `debugger` statement, under a chosen id.
        fn debugger_rule(id: &str) -> String {
            format!(
                "import {{ defineRule }} from 'lanekeep';\n\
                 export default defineRule({{\n\
                   id: '{id}',\n\
                   query: '(debugger_statement) @stmt',\n\
                   card: {{ message: 'debugger statement', remediation: 'remove it',\n\
                     examples: {{ bad: 'debugger;', good: 'x;' }} }},\n\
                   check(ctx, m) {{ ctx.report(m.stmt); }},\n\
                 }});\n"
            )
        }

        /// Every violation as `rule|file|line:column|message`, which is what an ordering
        /// assertion has to compare.
        fn rendered(outcome: &Outcome) -> Vec<String> {
            outcome
                .violations
                .iter()
                .map(|v| {
                    format!(
                        "{}|{}|{}:{}|{}",
                        v.rule_id,
                        v.location.file,
                        v.location.position.line,
                        v.location.position.column,
                        v.message
                    )
                })
                .collect()
        }

        #[test]
        fn a_component_rule_reports_at_the_node_its_query_captured() {
            // The whole dispatch path in one assertion: the query ran in Rust, the captures
            // crossed as a WIT `match`, the guest read the node's text through the host, and the
            // position on the violation is the one the query found rather than the root.
            let rule_a = debugger_rule("local/alpha");
            let project = Project::new(
                "component-basic",
                &[
                    ("rule-a.ts", &rule_a),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\nconst beta = 2;\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 1, false)])
                .expect("runs");

            assert_eq!(
                rendered(&outcome),
                vec![
                    "local/middle|src/a.ts|1:7|component saw `alpha`".to_owned(),
                    "local/middle|src/a.ts|2:7|component saw `beta`".to_owned(),
                ],
                "a component rule must report where its query matched"
            );
        }

        #[test]
        fn both_engines_run_in_one_corpus_and_feed_one_sorted_output() {
            // The property this task creates. Two dispatch paths, three rules, and one order.
            //
            // The component rule's id sorts *between* the two TypeScript rules and it is
            // declared *after* both, so an engine that ran one path and then the other and
            // concatenated would put it last. Sorting by `(ruleId, file, line, column)` is what
            // makes the two indistinguishable downstream.
            let alpha = debugger_rule("local/alpha");
            let zeta = debugger_rule("local/zeta");
            let project = Project::new(
                "component-mixed",
                &[
                    ("rule-a.ts", &alpha),
                    ("rule-z.ts", &zeta),
                    (
                        "lanekeep.config.ts",
                        &config_with(&["./rule-a", "./rule-z"]),
                    ),
                    ("src/a.ts", "const alpha = 1;\ndebugger;\n"),
                    ("src/b.ts", "debugger;\nconst beta = 2;\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 2, false)])
                .expect("runs");

            assert_eq!(
                rendered(&outcome),
                vec![
                    "local/alpha|src/a.ts|2:1|debugger statement".to_owned(),
                    "local/alpha|src/b.ts|1:1|debugger statement".to_owned(),
                    "local/middle|src/a.ts|1:7|component saw `alpha`".to_owned(),
                    "local/middle|src/b.ts|2:7|component saw `beta`".to_owned(),
                    "local/zeta|src/a.ts|2:1|debugger statement".to_owned(),
                    "local/zeta|src/b.ts|1:1|debugger statement".to_owned(),
                ],
                "the two engines' violations must interleave by id, not group by engine"
            );
        }

        #[test]
        fn a_mixed_run_is_byte_identical_to_itself() {
            // Determinism across the two paths, which is the invariant a second engine is most
            // likely to break: rayon assigns files to workers differently between runs, and the
            // component path adds a second source of per-worker state.
            let alpha = debugger_rule("local/alpha");
            let project = Project::new(
                "component-deterministic",
                &[
                    ("rule-a.ts", &alpha),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\ndebugger;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                    ("src/c.ts", "const gamma = 3;\ndebugger;\n"),
                    ("src/d.ts", "const delta = 4;\n"),
                ],
            );

            let first = rendered(
                &project
                    .run_with(vec![component_rule("local/middle", 1, true)])
                    .expect("runs"),
            );
            assert!(!first.is_empty(), "the fixture corpus produces violations");

            for round in 1..8 {
                let again = rendered(
                    &project
                        .run_with(vec![component_rule("local/middle", 1, true)])
                        .expect("runs"),
                );
                assert_eq!(again, first, "run {round} disagreed with the first");
            }
        }

        #[test]
        fn a_component_rules_facts_carry_their_file_in_the_field_and_not_in_the_payload() {
            // The engine-side duplicate-key hazard, and the only place it is visible.
            //
            // `lanekeep-js`'s reduce phase splices `"file"` into a fact's payload, because its
            // `ReduceFact` carries no file of its own. The world's `emitted-fact` has a `file`
            // field, so the component path fills that instead — and an engine that did both
            // would send a payload with two `"file"` keys. Nothing host-side would notice: the
            // host forwards `data` exactly as the guest wrote it. The fixture reports every
            // fact back as `kind|file|data`, which is what makes the payload assertable.
            let alpha = debugger_rule("local/alpha");
            let project = Project::new(
                "component-facts",
                &[
                    ("rule-a.ts", &alpha),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 1, true)])
                .expect("runs");

            let reduce_reports: Vec<&str> = outcome
                .violations
                .iter()
                .filter(|v| v.message.starts_with("seen|"))
                .map(|v| v.message.as_str())
                .collect();
            assert_eq!(
                reduce_reports,
                vec![
                    "seen|src/a.ts|{\"text\":\"alpha\"}",
                    "seen|src/b.ts|{\"text\":\"beta\"}",
                ],
                "a fact's file belongs in the record field, and the payload is the guest's"
            );

            for report in reduce_reports {
                let payload = report.rsplit('|').next().expect("a payload");
                assert!(
                    !payload.contains("\"file\""),
                    "the engine merged a file key into a payload that already had a field: \
                     {report}"
                );
            }
        }

        #[test]
        fn a_component_rules_cross_file_violations_are_reported_at_the_facts_file() {
            let project = Project::new(
                "component-reduce-site",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 1, true)])
                .expect("runs");

            let sites: Vec<String> = outcome
                .violations
                .iter()
                .filter(|v| v.message.starts_with("seen|"))
                .map(|v| format!("{}:{}", v.location.file, v.location.position.line))
                .collect();
            assert_eq!(sites, vec!["src/a.ts:1".to_owned()]);
        }

        #[test]
        fn a_gate_keeps_a_component_rule_off_a_file_exactly_as_it_does_a_module_rule() {
            // The gates run in Rust before either engine is reached, so a component must not
            // acquire a second answer to "does this rule run here".
            let mut gated = component_rule("local/middle", 1, false);
            gated.gates = lanekeep_core::Gates {
                file_contains: vec!["beta".to_owned()],
                ..lanekeep_core::Gates::default()
            };

            let project = Project::new(
                "component-gated",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                ],
            );

            let outcome = project.run_with(vec![gated]).expect("runs");
            assert_eq!(
                rendered(&outcome),
                vec!["local/middle|src/b.ts|1:7|component saw `beta`".to_owned()],
                "the content gate must exclude the file that does not hold the token"
            );
        }

        #[test]
        fn a_component_rule_does_not_run_on_a_language_it_does_not_declare() {
            // The grammar is chosen by the file and the rule declares which files it wants;
            // both engines apply the same gate, so a `.tsx` file is not checked by a rule that
            // names only `typescript`.
            let mut rule = component_rule("local/middle", 1, false);
            rule.languages = vec!["tsx".to_owned()];

            let project = Project::new(
                "component-language",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let outcome = project.run_with(vec![rule]).expect("runs");
            assert!(outcome.violations.is_empty(), "{:?}", rendered(&outcome));
        }

        #[test]
        fn swapping_a_component_between_runs_changes_the_answer() {
            // **This is a real staleness bug the shipped guard prevents, demonstrated rather
            // than argued.** `RuleSpec::component` is set on a `Config` *after*
            // `lanekeep_config::load` computed `ruleset_hash`, and `load_components` reads the
            // bytes with an untracked `std::fs::read` — so a component's bytes reach no
            // cache-key input at all. With the cache on and no guard, swapping the component
            // for a different one between two runs serves the first one's answer forever.
            //
            // Written against the copy the run actually loads, so the swap is the only
            // difference: same rule id, same path, same query, different bytes.
            let project = Project::new(
                "component-swap",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );
            let installed = project.dir.join("rule.wasm");
            fs::copy(fixture(), &installed).expect("installs the rule-shaped component");

            let mut rule = component_rule("local/middle", 1, false);
            rule.component = Some(installed.clone());

            let outcome = project
                .prepared(vec![rule.clone()])
                .expect("prepares")
                .run()
                .expect("runs");
            let first = messages(&outcome);
            assert!(first.contains(&"component saw `alpha`"), "{first:?}");

            // A different component at the same path. `limits.wasm` answers an unrecognized
            // probe by saying so, which is a message the first one cannot produce.
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../lanekeep-wasm/tests/fixtures/limits.wasm"),
                &installed,
            )
            .expect("swaps the component");

            let outcome = project
                .prepared(vec![rule])
                .expect("prepares")
                .run()
                .expect("runs");
            let second = messages(&outcome);
            assert!(
                second.iter().any(|m| m.contains("unknown probe")),
                "a swapped component must not be answered from the first one's cache: {second:?}"
            );
        }

        #[test]
        fn a_run_with_a_component_rule_does_not_touch_the_cache() {
            // The guard behind the test above, asserted directly rather than only through its
            // effect. Refusing the cache — rather than folding the component's bytes into the
            // key here — is deliberate: the correct fold already exists in `lanekeep-config`,
            // sorted, deduplicated and length-prefixed, and a second implementation of a
            // cache-key encoding in a second crate is precisely the drift that produced this
            // sub-project's one real cache bug. See `Engine::caching`.
            let project = Project::new(
                "component-no-cache",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let with_component = project
                .prepared(vec![component_rule("local/middle", 1, false)])
                .expect("prepares");
            assert!(
                !with_component.caching,
                "a run that loaded a component must not read or write the cache"
            );
            with_component.run().expect("runs");
            assert!(
                !Store::path_for(&project.dir).exists(),
                "and must leave no cache behind"
            );

            // The same project with no component rule caches exactly as it always did, so the
            // guard is scoped to the thing that is unsound rather than turning the cache off.
            let typescript_only = project.prepared(Vec::new()).expect("prepares");
            assert!(typescript_only.caching);
            typescript_only.run().expect("runs");
            assert!(Store::path_for(&project.dir).exists());
        }

        /// The same rule, with a query that also asks the fixture to burn real time first.
        ///
        /// A pattern-level capture beside the node-level one, so the guest receives both names
        /// and the violation still lands at `@target`.
        fn burning_rule(id: &str, timeout: Duration) -> RuleSpec {
            let mut rule = component_rule(id, 1, false);
            rule.query = format!("({QUERY}) @burn");
            rule.timeout = Some(timeout);
            rule
        }

        /// A project whose config sets the default per-invocation budget.
        fn burning_project(name: &str, default_timeout_ms: u64) -> Project {
            let config = format!(
                "import {{ defineConfig }} from 'lanekeep';\n\
                 import r0 from './rule-a';\n\
                 export default defineConfig({{ include: ['src/**/*.ts'], \
                 namespaces: ['local'], timeouts: {{ rule: {default_timeout_ms} }}, \
                 rules: [r0] }});\n"
            );
            Project::new(
                name,
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            )
        }

        #[test]
        fn a_component_rules_own_timeout_is_applied_and_not_merely_read() {
            // `AGENTS.md`'s "validating a flag is not applying it", asserted in both
            // directions, because only one of them discriminates. A rule declaring a *smaller*
            // budget than the config's aborts either way if the run is slow enough — so the
            // load-bearing half is the **raise**: a rule declaring a budget far larger than a
            // config default it would otherwise breach has to complete.
            //
            // The fixture burns real bytecode for this. A handler that returns immediately is
            // never asked to stop, because the budget is polled from epoch checks compiled into
            // guest code — so a fast fixture would pass against an engine that ignored the
            // value entirely.
            let raised = burning_project("component-timeout-raised", 20);
            raised
                .run_with(vec![burning_rule("local/middle", Duration::from_hours(1))])
                .expect("a rule that raised its own budget must complete");

            let lowered = burning_project("component-timeout-lowered", 3_600_000);
            let error = lowered
                .run_with(vec![burning_rule(
                    "local/middle",
                    Duration::from_millis(20),
                )])
                .expect_err("a rule that lowered its own budget must be stopped");
            assert!(matches!(error, RunError::Rule { .. }), "{error}");
            assert!(error.to_string().contains("local/middle"), "{error}");
        }

        #[test]
        fn the_runs_global_budget_reaches_a_component_rule() {
            // The clock is the run's, not the worker's and not the rule's. A component rule
            // that overruns the whole run's wall-clock budget has to be stopped by *that*
            // budget and say so, rather than being blamed for its own per-invocation one —
            // which it has not breached here, since it is given an hour.
            let config = "import { defineConfig } from 'lanekeep';\n\
                 import r0 from './rule-a';\n\
                 export default defineConfig({ include: ['src/**/*.ts'], \
                 namespaces: ['local'], timeouts: { global: 50 }, rules: [r0] });\n";
            let project = Project::new(
                "component-global-budget",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", config),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let error = project
                .run_with(vec![burning_rule("local/middle", Duration::from_hours(1))])
                .expect_err("the run's own budget must stop it")
                .to_string();
            assert!(
                error.contains("the run exceeded its"),
                "the global budget must be what is blamed, not the rule's: {error}"
            );
        }

        #[test]
        fn a_spent_run_budget_stops_a_component_rule_before_the_guest_is_entered() {
            // The same outer check as `run_budget`'s cases, on the other dispatch path — and it
            // is one check rather than two, which is the point: it sits in `check_file`, above
            // the `if let Some(slot)` that chooses an engine, so neither engine can be the one
            // that has it.
            //
            // What makes this discriminating is the *variant*. Measured against the commit
            // before this one, the same fixture failed with `RunError::Rule` — the epoch
            // mechanism noticed, mid-instantiation, and blamed `local/middle` for `src/a.ts`.
            // That is a rule and a file named for a breach that is about neither, and it is
            // only luck that anything noticed at all: `AGENTS.md` records that epoch checks
            // live inside guest code, so a tick that lands between two calls is invisible to
            // them. `RunError::RunTimeout` can only come from the walker.
            //
            // A budget of zero is a run whose clock is spent before the first file, which is
            // the one arrangement in which nothing but the outer check can fire — the guest is
            // never entered, so there is no epoch deadline to trip. `lanekeep-wasm`'s own
            // limit tests avoid a born-expired clock for the opposite reason, that
            // instantiation is itself a budgeted guest call; here that is exactly what must
            // not happen.
            let config = "import { defineConfig } from 'lanekeep';\n\
                 import r0 from './rule-a';\n\
                 export default defineConfig({ include: ['src/**/*.ts'], \
                 namespaces: ['local'], timeouts: { global: 0 }, rules: [r0] });\n";
            let project = Project::new(
                "component-spent-budget",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", config),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let error = project
                .run_with(vec![component_rule("local/middle", 1, false)])
                .expect_err("a spent run budget stops the run");
            assert!(
                matches!(error, RunError::RunTimeout { .. }),
                "the walker had to stop this before any guest ran: {error}"
            );
        }

        #[test]
        fn each_reducing_component_rule_sees_only_its_own_facts() {
            // A rule reading another's facts would make an internal payload shape into a
            // contract between rules, and would make a result depend on the order rules were
            // declared in. Two reducing rules is the smallest case that can tell the filter
            // from its absence — with one, "its own facts" and "every fact" are the same list.
            let project = Project::new(
                "component-fact-isolation",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                ],
            );

            let outcome = project
                .run_with(vec![
                    component_rule("local/first", 1, true),
                    component_rule("local/second", 2, true),
                ])
                .expect("runs");

            for id in ["local/first", "local/second"] {
                let seen: Vec<&str> = outcome
                    .violations
                    .iter()
                    .filter(|v| v.rule_id.to_string() == id && v.message.starts_with("seen|"))
                    .map(|v| v.message.as_str())
                    .collect();
                assert_eq!(
                    seen,
                    vec![
                        "seen|src/a.ts|{\"text\":\"alpha\"}",
                        "seen|src/b.ts|{\"text\":\"beta\"}",
                    ],
                    "`{id}` must see its own two facts and not the other rule's as well"
                );
            }
        }

        #[test]
        fn a_profiled_run_walks_the_tree_per_component_rule_and_agrees_with_the_shared_pass() {
            // `--profile` turns the one-traversal-per-file pass off, because the per-rule split
            // it reports cannot be divided honestly between rules that share a traversal. So
            // there is a second, otherwise untested path into a component rule: the rule walks
            // the tree alone, through the context's own arena.
            //
            // Two claims, and the second is what makes the first worth having: the answers are
            // the same as the shared pass produces, and the language gate still applies — which
            // on the shared pass is enforced by the combined query having no pattern for this
            // rule at all, and here is enforced by nothing but the check itself.
            let project = Project::new(
                "component-profiled",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let outcome = project
                .prepared(vec![component_rule("local/middle", 1, false)])
                .expect("prepares")
                .without_cache()
                .profiling()
                .run()
                .expect("runs");
            assert_eq!(
                rendered(&outcome),
                vec!["local/middle|src/a.ts|1:7|component saw `alpha`".to_owned()],
                "the per-rule walk must agree with the shared traversal"
            );
            let timings = outcome.timings.expect("profiling collects timings");
            let middle = timings
                .get(&"local/middle".parse::<RuleId>().expect("a rule id"))
                .expect("the component rule is timed like any other");
            assert_eq!(middle.matches, 1, "the match count comes from the walk");

            // A rule declaring *several* languages, with the file's second in the list. This is
            // the case that distinguishes "the grammar the file chose" from "the first grammar
            // the rule compiled": both are present, only one parses this tree, and a query
            // compiled against the other matches nothing at all — silently, which is exactly
            // the failure mode `AGENTS.md` records from the `.tsx` migration.
            let mut both = component_rule("local/middle", 1, false);
            both.languages = vec!["tsx".to_owned(), "typescript".to_owned()];
            let outcome = project
                .prepared(vec![both])
                .expect("prepares")
                .without_cache()
                .profiling()
                .run()
                .expect("runs");
            assert_eq!(
                rendered(&outcome),
                vec!["local/middle|src/a.ts|1:7|component saw `alpha`".to_owned()],
                "the grammar is the file's, not the first one the rule happened to declare"
            );

            // The same rule, declaring a language this file is not. On the profiled path the
            // combined query is not built, so the only thing keeping it off the file is the
            // gate in the dispatch itself.
            let mut elsewhere = component_rule("local/middle", 1, false);
            elsewhere.languages = vec!["tsx".to_owned()];
            let outcome = project
                .prepared(vec![elsewhere])
                .expect("prepares")
                .without_cache()
                .profiling()
                .run()
                .expect("runs");
            assert!(
                outcome.violations.is_empty(),
                "a rule that does not name this file's language must not run on it: {:?}",
                rendered(&outcome)
            );
        }

        #[test]
        fn a_worker_whose_store_has_trapped_keeps_reporting_what_went_wrong() {
            // `bindgen!` is configured with `imports: { default: trappable }`, so a trap sets a
            // store-wide flag with no public reset: the *next* call on that store fails with
            // wasmtime's `cannot enter component instance`, which describes the runtime's
            // bookkeeping rather than anything that went wrong. rayon keeps handing this worker
            // its remaining files, and which of several failures surfaces from the reduction is
            // arbitrary — so a run could be reported against a file that was fine, with a
            // message naming nothing.
            //
            // Nothing is rescued by noticing: every failure here cancels the run either way.
            // What is rescued is the diagnostic, and this is the assertion that it is.
            let project = Project::new(
                "component-poisoned",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                ],
            );

            // The `limits` fixture spins forever when the first capture is named `spin`, which
            // is the shortest route to a store that has trapped.
            let mut spinner = component_rule("local/middle", 1, false);
            spinner.component = Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../lanekeep-wasm/tests/fixtures/limits.wasm"),
            );
            spinner.query = "(variable_declarator) @spin".to_owned();
            spinner.timeout = Some(Duration::from_millis(30));

            let engine = project.engine(vec![spinner]).without_cache();
            let clock = RunClock::start(engine.limits.global_timeout);
            let cache = Store::empty();
            let mut worker = Worker::new(&engine, &clock);

            let files = engine.discover();
            let Err(first) = engine.check_file(&mut worker, &cache, &files[0]) else {
                panic!("a spinning rule must breach its budget")
            };
            let Err(second) = engine.check_file(&mut worker, &cache, &files[1]) else {
                panic!("the store has trapped and cannot be entered again")
            };

            assert_eq!(
                second.to_string(),
                first.to_string(),
                "the second file must be told what actually went wrong"
            );
            assert!(
                !second
                    .to_string()
                    .contains("cannot enter component instance"),
                "{second}"
            );
        }

        #[test]
        fn a_missing_component_is_reported_against_its_rule_before_any_file_is_read() {
            let mut rule = component_rule("local/middle", 1, false);
            rule.component = Some(PathBuf::from("/does/not/exist/rule.wasm"));

            let project = Project::new(
                "component-missing",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let error = project.run_with(vec![rule]).expect_err("must not run");
            let rendered = error.to_string();
            assert!(matches!(error, RunError::Component { .. }), "{rendered}");
            assert!(rendered.contains("local/middle"), "{rendered}");
        }

        #[test]
        fn a_component_that_is_not_a_component_is_refused() {
            let project = Project::new(
                "component-garbage",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("not-a-rule.wasm", "this is not WebAssembly"),
                ],
            );

            let mut rule = component_rule("local/middle", 1, false);
            rule.component = Some(project.dir.join("not-a-rule.wasm"));

            let error = project.run_with(vec![rule]).expect_err("must not run");
            assert!(matches!(error, RunError::Component { .. }), "{error}");
        }

        #[test]
        fn a_run_with_no_component_rule_builds_no_component_engine() {
            // Building one spawns the epoch ticker thread that enforces both wall-clock
            // budgets, and compiles nothing. Every run this tree can express today is this one,
            // so "beside" has to mean "and costs nothing when unused".
            let project = Project::new(
                "component-absent",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "debugger;\n"),
                ],
            );

            let engine = project.engine(Vec::new());
            assert!(
                engine.components.is_none(),
                "a TypeScript-only ruleset must not build a component engine"
            );
        }

        #[test]
        fn a_worker_instantiates_a_component_rule_once_however_many_files_it_handles() {
            // The bound `MEMORY_RESERVATION` is chosen on: one instance per (worker, rule).
            // Driven through one `Worker` directly rather than through `run`, because rayon
            // decides how many workers exist and the claim is about one of them.
            let project = Project::new(
                "component-instantiations",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("src/b.ts", "const beta = 2;\n"),
                    ("src/c.ts", "const gamma = 3;\n"),
                ],
            );

            let engine = project
                .engine(vec![component_rule("local/middle", 1, false)])
                .without_cache();
            let clock = RunClock::start(engine.limits.global_timeout);
            let cache = Store::empty();
            let mut worker = Worker::new(&engine, &clock);

            for path in engine.discover() {
                engine
                    .check_file(&mut worker, &cache, &path)
                    .expect("checks");
            }

            let runtime = worker.wasm.as_ref().expect("a component rule ran");
            assert_eq!(
                runtime.instantiations(),
                1,
                "three files sharing one worker must instantiate the rule once"
            );
            assert!(
                runtime.host().holds_no_contexts(),
                "each file's context must be given back, or a worker's store grows with the \
                 corpus"
            );
        }

        #[test]
        fn a_worker_whose_component_rules_never_match_instantiates_nothing() {
            // The case eager instantiation pays 82 to 96 times over for. A worker that never
            // reaches a match must not build a store's worth of instances.
            let project = Project::new(
                "component-unmatched",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "debugger;\n"),
                ],
            );

            let engine = project
                .engine(vec![component_rule("local/middle", 1, false)])
                .without_cache();
            let clock = RunClock::start(engine.limits.global_timeout);
            let cache = Store::empty();
            let mut worker = Worker::new(&engine, &clock);

            for path in engine.discover() {
                engine
                    .check_file(&mut worker, &cache, &path)
                    .expect("checks");
            }

            assert!(
                worker.wasm.is_none(),
                "a worker with no component match must not build a store at all"
            );
        }

        #[test]
        fn one_read_memo_serves_both_engines_over_one_file() {
            // The hazard a shared `FileAccess` closes. Two rules on one file, one in each
            // engine, both reading the same path: with one memo per engine the second reader
            // sees whatever is on disk *now*, and the two dependency lists disagree about the
            // path's hash — which `tracked::sort` cannot repair, because it orders by path and
            // does not dedupe.
            //
            // Asserted on the recorded dependency list rather than on what a rule saw, because
            // that list is the cache-entry input and a duplicate in it is a cache entry that can
            // never be validated.
            const READER: &str = "import { defineRule } from 'lanekeep';\n\
                export default defineRule({\n\
                  id: 'local/alpha',\n\
                  query: '(variable_declarator) @d',\n\
                  card: { message: 'read', remediation: 'x',\n\
                    examples: { bad: 'a', good: 'b' } },\n\
                  check(ctx, m) { ctx.readFile('shared.json'); ctx.report(m.d); },\n\
                });\n";

            let project = Project::new(
                "component-one-memo",
                &[
                    ("rule-a.ts", READER),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("shared.json", "{\"v\":1}\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 1, false)])
                .expect("runs");

            let reads = outcome
                .dependencies
                .get(&FilePath::new("src/a.ts"))
                .expect("the file recorded a tracked read");
            assert_eq!(
                reads.len(),
                1,
                "one path read once must be one dependency, not one per engine: {reads:?}"
            );
            assert_eq!(reads[0].path.as_str(), "shared.json");
            assert!(reads[0].hash.is_some(), "the file was there and was read");
        }

        #[test]
        fn two_component_rules_on_one_file_share_one_context() {
            // Found by mutation: every other test here has exactly one component rule, so
            // "one context per file" and "one context per rule" are the same arrangement and
            // nothing could tell them apart. Two rules on one file is the smallest case where
            // they differ.
            //
            // What per-file buys is the arena, the query cache and a single entry in the
            // store's table. The table is what an assertion can reach: a per-rule context
            // would replace the file's entry without deleting it, so the first rule's arena —
            // the parse tree and the file's whole source — would be stranded in a store that
            // lives for the rest of the worker's share of the corpus.
            let project = Project::new(
                "component-two-rules",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                ],
            );

            let engine = project
                .engine(vec![
                    component_rule("local/first", 1, false),
                    component_rule("local/second", 2, false),
                ])
                .without_cache();
            let clock = RunClock::start(engine.limits.global_timeout);
            let cache = Store::empty();
            let mut worker = Worker::new(&engine, &clock);

            let mut reported = Vec::new();
            for path in engine.discover() {
                reported.extend(
                    engine
                        .check_file(&mut worker, &cache, &path)
                        .expect("checks")
                        .violations,
                );
            }

            // Both rules ran, and each got its own reports rather than one of them collecting
            // the other's — the context is shared, so attribution comes from taking the reports
            // between rules and not from the context.
            let mut ids: Vec<String> = reported.iter().map(|v| v.rule_id.to_string()).collect();
            ids.sort();
            assert_eq!(
                ids,
                vec!["local/first".to_owned(), "local/second".to_owned()],
                "both component rules must report, once each"
            );

            let runtime = worker.wasm.as_ref().expect("a component rule ran");
            assert!(
                runtime.host().holds_no_contexts(),
                "two rules on one file must leave one context behind, and it must be given back"
            );
            assert_eq!(
                runtime.instantiations(),
                2,
                "two rules is two instances, and two rules on one file is still two"
            );
        }

        #[test]
        fn a_component_rules_read_reaches_the_dependency_list_at_all() {
            // The half of the shared-memo claim that a duplicate count cannot make. If the
            // component engine held a `FileAccess` of its own, its reads would be recorded
            // against a memo the engine never reads back — so they would not be duplicated,
            // they would be *gone*, and the file's cache entry would not be invalidated when
            // the file it depended on changed. Only a component rule reads here, so the entry
            // exists if and only if the two engines share one access.
            let project = Project::new(
                "component-only-reader",
                &[
                    ("rule-a.ts", &debugger_rule("local/alpha")),
                    ("lanekeep.config.ts", &config_with(&["./rule-a"])),
                    ("src/a.ts", "const alpha = 1;\n"),
                    ("shared.json", "{\"v\":1}\n"),
                ],
            );

            let outcome = project
                .run_with(vec![component_rule("local/middle", 1, false)])
                .expect("runs");

            let reads = outcome
                .dependencies
                .get(&FilePath::new("src/a.ts"))
                .expect("a component rule's tracked read must reach the dependency list");
            assert_eq!(reads.len(), 1, "{reads:?}");
            assert_eq!(reads[0].path.as_str(), "shared.json");
        }

        #[test]
        fn a_run_that_binds_an_undeclared_interface_cannot_be_assembled() {
            // **The wiring, not the comparison.** The check used to be a statement in
            // `load_components`, and deleting that statement left all one hundred engine tests
            // passing with no dead-code warning, because the test below calls the comparison
            // directly. So this drives a real `RuleSet` through the real recording path —
            // `linker_mut` takes the declaration and pushes it — and then through the only
            // constructor a run's component set has.
            let engine = WasmEngine::new().expect("the runtime builds");
            let mut set = RuleSet::new(&engine).expect("the world links");
            // The linker itself is not wanted — what is under test is that reaching for it
            // records the declaration, which is what `linker_mut` does on the way.
            let _ = set.linker_mut(&ExternalBinding::declare(
                "wasi:random/random",
                "a fixed 64-byte cycle, all zeroes",
            ));

            let error = Components::linked(Arc::clone(&engine), set)
                .err()
                .expect("a set that bound something undeclared must not become a run")
                .to_string();
            assert!(error.contains("wasi:random/random"), "{error}");

            // And a set that bound nothing assembles, so the refusal is about the binding rather
            // than about component runs in general.
            let clean = RuleSet::new(&engine).expect("the world links");
            assert!(Components::linked(engine, clean).is_ok());
        }

        #[test]
        fn a_declaration_that_does_not_match_the_cache_keys_own_list_stops_the_run() {
            // `EXTERNAL_BINDINGS` was a signature and nothing compared it against what a run
            // actually bound. A binding declared at a call site and left out of the constant is
            // a run whose rules reach something no cached result knows about, with every
            // cache-key input identical.
            assert!(
                declared_bindings_match(EXTERNAL_BINDINGS).is_ok(),
                "a run that binds exactly the declared list is accepted"
            );

            let undeclared = [ExternalBinding::declare(
                "wasi:random/random",
                "a fixed 64-byte cycle, all zeroes",
            )];
            let error = declared_bindings_match(&undeclared)
                .expect_err("an undeclared binding must stop the run")
                .to_string();
            assert!(error.contains("wasi:random/random"), "{error}");
            assert!(error.contains("EXTERNAL_BINDINGS"), "{error}");
        }
    }
}
