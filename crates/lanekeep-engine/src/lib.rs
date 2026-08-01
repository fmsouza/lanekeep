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
use std::rc::Rc;
use std::sync::Arc;

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

    /// A rule's gates are malformed.
    #[error("rule `{rule}` has invalid gates: {detail}")]
    Gates {
        /// Which rule.
        rule: String,
        /// What is wrong.
        detail: String,
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
struct Prepared {
    spec: RuleSpec,
    query: CompiledQuery,
    gates: CompiledGates,
    language: Arc<dyn Language>,
}

/// Everything a run needs, built once and shared across workers.
pub struct Engine {
    rules: Vec<Prepared>,
    discovery: Discovery,
    /// The project root, canonicalized once. Every tracked read is checked against it, and
    /// canonicalizing per file would put a syscall on the hot path for a constant.
    root: PathBuf,
    /// Everything constant about this run that a cache key depends on.
    run_key: RunKey,
    /// Whether results may be read from and written to the cache.
    caching: bool,
    /// Whether reduce phases run.
    reducing: bool,
    /// Whether directives that silenced nothing are reported.
    reporting_unused: bool,
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
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("rules", &self.rules.len())
            .field("root", &self.discovery.root())
            .finish_non_exhaustive()
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

        let mut rules = Vec::with_capacity(config.rules.len());
        for spec in &config.rules {
            if !spec.severity.is_enabled() {
                continue;
            }

            let language = registry.by_id(&spec.language).cloned().ok_or_else(|| {
                RunError::UnknownLanguage {
                    rule: spec.id.to_string(),
                    language: spec.language.clone(),
                    known: known.clone(),
                }
            })?;

            let query = CompiledQuery::compile(language.as_ref(), &spec.query).map_err(
                |e: CompileError| RunError::Query {
                    rule: spec.id.to_string(),
                    detail: e.to_string(),
                },
            )?;

            let gates = CompiledGates::compile(&spec.gates).map_err(|e| RunError::Gates {
                rule: spec.id.to_string(),
                detail: e.to_string(),
            })?;

            rules.push(Prepared {
                spec: spec.clone(),
                query,
                gates,
                language,
            });
        }

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

        let run_key = RunKey::new(
            // Major.minor only: a patch release changes nothing a rule can observe, and
            // invalidating every cache on one would make patch upgrades expensive for
            // nothing.
            engine_version(),
            HOST_API_VERSION,
            &config.ruleset_hash,
            &config.config_hash,
            &grammars,
        );

        Ok(Self {
            rules,
            run_key,
            caching: true,
            reducing: true,
            reporting_unused: false,
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
        })
    }

    /// Turn the cache off, for `--no-cache` and for tests that need a cold run.
    #[must_use]
    pub const fn without_cache(mut self) -> Self {
        self.caching = false;
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
        for result in results {
            let outcome = result?;
            violations.extend(outcome.violations);
            facts.extend(outcome.facts);
            files_parsed += usize::from(outcome.parsed);
            if let Some(entry) = outcome.entry {
                fresh.insert(entry.0, entry.1);
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

        if self.caching {
            match coverage {
                // The run saw everything, so what it did not produce an entry for no longer
                // exists. Saving only fresh entries is what ages deleted files out.
                Coverage::Whole => fresh.save(&self.root),
                // The run saw a subset. Saving only what it produced would discard the
                // entries for every file it never looked at — so `--staged` would leave the
                // next full run cold, which is the opposite of what an incremental entry
                // point is for.
                Coverage::Partial => {
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

        let sandbox = self.build_sandbox(clock)?;
        let paths: Vec<String> = files.iter().map(|f| f.as_str().to_owned()).collect();
        let mut violations = Vec::new();

        for rule in reducing {
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
        // A fresh set of tracked reads for this file, sharing the root already canonicalized
        // at preparation.
        let files = Rc::new(FileAccess::rooted(self.root.clone()));

        // Path gates first: rejecting here costs no read at all.
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
        for rule in admitted {
            let (violations, facts, read_the_date) =
                self.run_rule(worker, &files, rule, path, &source)?;
            outcome.violations.extend(violations);
            outcome.facts.extend(facts);
            outcome.read_the_date |= read_the_date;
        }

        // Applied after every rule has run, so a directive covers whatever any of them
        // reported at that line. Which directive fired is recorded rather than discarded:
        // it is the only moment the information exists, since a warm run sees the survivors
        // and not what was hidden.
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
            .extend(self.directive_violations(&directives, path));

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

    fn run_rule(
        &self,
        worker: &mut Worker<'_>,
        files: &Rc<FileAccess>,
        rule: &Prepared,
        path: &FilePath,
        source: &str,
    ) -> Result<(Vec<Violation>, Vec<Fact>, bool), RunError> {
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&rule.language.grammar()).is_err() {
            return Ok((Vec::new(), Vec::new(), false));
        }
        let Some(tree) = parser.parse(source, None) else {
            return Ok((Vec::new(), Vec::new(), false));
        };

        // Collect capture paths while the tree is borrowed, then intern once the borrow
        // has ended — the two-phase shape the arena's ownership of the tree forces.
        let mut matches: Vec<Vec<(String, Vec<u32>)>> = Vec::new();
        let host = HostContext::new(tree, source.to_owned(), path.as_str())
            .with_resolver_from(rule.language.as_ref())
            .with_language(Arc::clone(&rule.language))
            .with_today(&self.today.to_string())
            .with_file_access(Rc::clone(files));

        {
            let arena = host.arena().borrow();
            rule.query
                .for_each_match(arena.tree(), source.as_bytes(), |m| {
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

        if matches.is_empty() {
            return Ok((Vec::new(), Vec::new(), false));
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

            sandbox
                .eval_with_host_timeout::<()>(&host, &call, timeout)
                .map_err(|e: SandboxError| RunError::Rule {
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

        Ok((violations, facts, host.date_was_read()))
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
}

impl<'a> Worker<'a> {
    fn new(engine: &'a Engine, clock: &Arc<RunClock>) -> Self {
        Self {
            engine,
            clock: Arc::clone(clock),
            sandbox: None,
            failed: None,
        }
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

    fn config(extra: &str) -> String {
        format!(
            "import {{ defineConfig }} from 'lanekeep';\n\
             import rule from './rule';\n\
             export default defineConfig({{ include: ['src/**/*.ts'], rules: [rule]{extra} }});\n"
        )
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
}
