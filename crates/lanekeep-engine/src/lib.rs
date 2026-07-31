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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lanekeep_config::{Config, ConfigError, RuleSpec};
use lanekeep_core::{
    CompiledGates, Discovery, DiscoveryError, FilePath, Location, Position, Severity, Violation,
};
use lanekeep_js::{HostContext, Limits, RuleRoot, RunClock, Sandbox, SandboxError};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Violations, in canonical order.
    pub violations: Vec<Violation>,
    /// How many files discovery selected.
    pub files_discovered: usize,
    /// How many were actually parsed, after gates.
    pub files_parsed: usize,
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

        Ok(Self {
            rules,
            discovery,
            limits: config.limits,
            rules_root,
            config_path: config_path.to_path_buf(),
            typescript,
            javascript,
        })
    }

    /// How many rules will actually run. Rules set to `off` are dropped at preparation.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
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
        self.run_over(&files)
    }

    /// Run over an explicit file list, for `--since` and `--staged`.
    ///
    /// # Errors
    ///
    /// As [`Engine::run`].
    pub fn run_over(&self, files: &[FilePath]) -> Result<Outcome, RunError> {
        let clock = RunClock::start(self.limits.global_timeout);

        let results: Vec<Result<(Vec<Violation>, bool), RunError>> = files
            .par_iter()
            .map_init(
                // One sandbox per worker, created on first use and reused for that
                // worker's whole share. Building one per file would pay engine startup
                // thousands of times; sharing one across workers is impossible, since the
                // runtime is single-threaded by construction.
                || self.worker(&clock),
                |worker, path| match worker {
                    Ok(sandbox) => self.check_file(sandbox, path),
                    Err(e) => Err(e.clone()),
                },
            )
            .collect();

        let mut violations = Vec::new();
        let mut files_parsed = 0;
        for result in results {
            let (found, parsed) = result?;
            violations.extend(found);
            files_parsed += usize::from(parsed);
        }

        lanekeep_core::sort(&mut violations);
        Ok(Outcome {
            violations,
            files_discovered: files.len(),
            files_parsed,
        })
    }

    fn worker(&self, clock: &Arc<RunClock>) -> Result<Sandbox, RunError> {
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

    /// Check one file. Returns its violations and whether it was parsed at all.
    fn check_file(
        &self,
        sandbox: &Sandbox,
        path: &FilePath,
    ) -> Result<(Vec<Violation>, bool), RunError> {
        // Path gates first: rejecting here costs no read at all.
        let admitted: Vec<&Prepared> = self
            .rules
            .iter()
            .filter(|rule| rule.gates.admits_path(path))
            .collect();
        if admitted.is_empty() {
            return Ok((Vec::new(), false));
        }

        let absolute = self.discovery.root().join(path.as_str());
        let Ok(bytes) = std::fs::read(&absolute) else {
            // A file that vanished between discovery and reading is not a failure. The
            // tree is allowed to change under a run; what must not happen is a partial
            // result being reported as complete, and a missing file contributes nothing
            // either way.
            return Ok((Vec::new(), false));
        };

        // Content gates: one read, a substring scan, and a parse saved.
        let admitted: Vec<&Prepared> = admitted
            .into_iter()
            .filter(|rule| rule.gates.admits_content(&bytes))
            .collect();
        if admitted.is_empty() {
            return Ok((Vec::new(), false));
        }

        let Ok(source) = String::from_utf8(bytes) else {
            // Not valid UTF-8, so not source this tool can reason about.
            return Ok((Vec::new(), false));
        };

        let mut violations = Vec::new();
        for rule in admitted {
            violations.extend(self.run_rule(sandbox, rule, path, &source)?);
        }

        Ok((violations, true))
    }

    fn run_rule(
        &self,
        sandbox: &Sandbox,
        rule: &Prepared,
        path: &FilePath,
        source: &str,
    ) -> Result<Vec<Violation>, RunError> {
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&rule.language.grammar()).is_err() {
            return Ok(Vec::new());
        }
        let Some(tree) = parser.parse(source, None) else {
            return Ok(Vec::new());
        };

        // Collect capture paths while the tree is borrowed, then intern once the borrow
        // has ended — the two-phase shape the arena's ownership of the tree forces.
        let mut matches: Vec<Vec<(String, Vec<u32>)>> = Vec::new();
        let host = HostContext::new(tree, source.to_owned(), path.as_str())
            .with_resolver_from(rule.language.as_ref());

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
            return Ok(Vec::new());
        }

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

        for report in host.take_reports() {
            violations.push(Violation {
                rule_id: rule.spec.id.clone(),
                location: Location::new(path.clone(), Position::new(report.line, report.column)),
                message: report
                    .message
                    .unwrap_or_else(|| rule.spec.card.message.clone()),
                remediation: rule.spec.card.remediation.clone(),
                severity: rule.spec.severity,
            });
        }

        Ok(violations)
    }
}

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
}
