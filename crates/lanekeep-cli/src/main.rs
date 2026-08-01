//! Command-line interface for lanekeep.
//!
//! See `docs/architecture.md` §12 for the command surface.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use lanekeep_core::FilePath;
use std::collections::BTreeMap;

use lanekeep_engine::{Engine, Outcome};
use lanekeep_js::RuleRoot;
use lanekeep_lang_js::{JavaScript, TypeScript};
use lanekeep_report::{Color, Format, Summary};

/// Exit code for a runtime error, per §11.
///
/// Distinct from `1`, which means the run completed and found violations. A caller has to
/// be able to tell "your code has problems" from "the checker could not run" — a
/// pre-commit hook that treats them the same either blocks on a broken install or lets
/// violations through when the tool crashes.
const EXIT_RUNTIME_ERROR: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "lanekeep",
    version,
    about = "Deterministic, AST-based architectural conformance checking"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check a project against its rules.
    Check {
        /// Project root. Defaults to the current directory.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Config file. Defaults to `lanekeep.config.ts` in the project root.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Output format.
        #[arg(long, default_value = "human")]
        format: String,

        /// Report violations but exit 0, for a phased rollout.
        #[arg(long)]
        warn_only: bool,

        /// Global wall-clock budget in milliseconds.
        #[arg(long)]
        timeout: Option<u64>,

        /// Recompute everything, ignoring and not writing the cache.
        ///
        /// For diagnosing a suspected stale result. If one is ever found this way, the
        /// cache key is missing an input — that is a bug, not a reason to keep the flag on.
        #[arg(long)]
        no_cache: bool,

        /// Check only files changed against a git ref.
        #[arg(long, value_name = "REF", conflicts_with = "staged")]
        since: Option<String>,

        /// Check only files staged in the index. The pre-commit default.
        #[arg(long)]
        staged: bool,

        /// Report where the run spent its time, per rule.
        ///
        /// The split between query matching and handler execution is what tells an author
        /// whether their query or their code is the problem.
        #[arg(long)]
        profile: bool,

        /// Apply the safe fixes rules offered, then report what is left.
        ///
        /// Only fixes a rule marked as preserving behavior. A fix it did not mark is a
        /// suggestion, and a suggestion is shown rather than applied.
        #[arg(long)]
        fix: bool,

        /// Also report suppressions that silenced nothing.
        ///
        /// Hygiene: a suppression whose violation no longer exists documents a decision
        /// about code that has changed, and nothing else will ever say so.
        #[arg(long)]
        report_unused_suppressions: bool,
    },

    /// Write a starter config and a first rule.
    Init {
        /// Project root. Defaults to the current directory.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Overwrite an existing config.
        #[arg(long)]
        force: bool,
    },

    /// List the rules a project has configured.
    Rules {
        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Config file.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Emit the listing as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Print one rule's card: what it checks, and what to do about it.
    Explain {
        /// The rule id, as it appears in output — `lanekeep/no-default-export`.
        rule: String,

        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Config file.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Emit the card as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            // Errors go to stderr, so a caller piping `--format json` into a parser gets
            // only the document on stdout whatever happens.
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "lanekeep: {error:#}");
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    match Cli::parse().command {
        Command::Check {
            path,
            config,
            format,
            warn_only,
            timeout,
            no_cache,
            since,
            staged,
            report_unused_suppressions,
            fix,
            profile,
        } => check(CheckOptions {
            project_root: &path,
            config: config.as_deref(),
            format: &format,
            timeout,
            selection: Selection::from(since, staged),
            switches: Switches {
                warn_only,
                no_cache,
                report_unused_suppressions,
                fix,
                profile,
            },
        }),
        Command::Init { path, force } => init(&path, force),
        Command::Rules { path, config, json } => rules(&path, config.as_deref(), json),
        Command::Explain {
            rule,
            path,
            config,
            json,
        } => explain(&rule, &path, config.as_deref(), json),
    }
}

/// Apply the safe fixes, say what happened, and check again.
///
/// Checked again because what a fix leaves behind is a different file: reporting the pre-fix
/// violations would list things that are no longer there. The second pass is a cache miss
/// for exactly the files that changed, which is what makes it cheap.
fn fix_and_recheck(
    project_root: &Path,
    config: Option<&Path>,
    caching: bool,
    outcome: Outcome,
) -> anyhow::Result<Outcome> {
    let written = apply_fixes(project_root, &outcome)?;
    if written.files == 0 {
        return Ok(outcome);
    }

    let mut stderr = std::io::stderr();
    writeln!(
        stderr,
        "fixed {} violation(s) in {} file(s)",
        written.applied, written.files
    )?;
    if written.skipped > 0 {
        // Never silent. A run that fixed three of five things and said it fixed everything
        // would leave someone believing the file was clean.
        writeln!(
            stderr,
            "  {} fix(es) skipped: another fix had already claimed those bytes",
            written.skipped
        )?;
    }

    let (engine, _) = prepare(project_root, config, caching)?;
    engine.run().map_err(|e| anyhow::anyhow!("{e}"))
}

/// What `--fix` wrote.
struct Written {
    files: usize,
    applied: usize,
    skipped: usize,
}

/// Apply every safe fix, grouped by file.
///
/// Writes happen only here, only to files a rule reported on, and only within the ranges of
/// nodes those rules matched. A file whose fixes all turn out to be suggestions is not
/// rewritten at all — not even with identical bytes, which would still update its mtime and
/// make it look changed to everything else watching the tree.
fn apply_fixes(project_root: &Path, outcome: &Outcome) -> anyhow::Result<Written> {
    let mut by_file: BTreeMap<&str, Vec<lanekeep_core::Fix>> = BTreeMap::new();
    for violation in &outcome.violations {
        if let Some(fix) = &violation.fix {
            by_file
                .entry(violation.location.file.as_str())
                .or_default()
                .push(fix.clone());
        }
    }

    let mut written = Written {
        files: 0,
        applied: 0,
        skipped: 0,
    };

    for (file, fixes) in by_file {
        let path = project_root.join(file);
        let Ok(source) = std::fs::read_to_string(&path) else {
            // Vanished, or not text any more. The tree is allowed to change under a run;
            // what must not happen is writing to a file this no longer understands.
            continue;
        };

        let result = lanekeep_core::fix::apply(&source, &fixes);
        written.skipped += result.skipped;
        if result.applied == 0 {
            continue;
        }

        std::fs::write(&path, &result.source)
            .map_err(|e| anyhow::anyhow!("cannot write `{}`: {e}", path.display()))?;
        written.files += 1;
        written.applied += result.applied;
    }

    Ok(written)
}

/// Which files to check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    /// Everything discovery finds.
    All,
    /// Files changed against a git ref.
    Since(String),
    /// Files staged in the index.
    Staged,
}

impl Selection {
    fn from(since: Option<String>, staged: bool) -> Self {
        match (since, staged) {
            (Some(reference), _) => Self::Since(reference),
            (None, true) => Self::Staged,
            (None, false) => Self::All,
        }
    }

    const fn is_narrowed(&self) -> bool {
        !matches!(self, Self::All)
    }

    /// How the flag would be written, for a message that has to name it.
    const fn flag(&self) -> &'static str {
        match self {
            Self::All => "",
            Self::Since(_) => "--since",
            Self::Staged => "--staged",
        }
    }

    /// The files this selection names, or `None` for everything.
    ///
    /// # Errors
    ///
    /// Fails if git cannot answer.
    fn resolve(&self, project_root: &Path) -> anyhow::Result<Option<Vec<FilePath>>> {
        match self {
            Self::All => Ok(None),
            Self::Since(reference) => Ok(Some(lanekeep_core::changed::since(
                project_root,
                reference,
            )?)),
            Self::Staged => Ok(Some(lanekeep_core::changed::staged(project_root)?)),
        }
    }
}

/// Print where the run spent its time, per rule.
///
/// Sorted by total cost, because the reason to ask is to find the expensive one. Ties break
/// on rule id so the table does not reorder between runs over the same corpus for reasons
/// nobody can see.
fn write_profile(
    timings: &BTreeMap<lanekeep_core::RuleId, lanekeep_engine::RuleTiming>,
) -> anyhow::Result<()> {
    let mut ranked: Vec<_> = timings.iter().collect();
    ranked.sort_by(|(a_id, a), (b_id, b)| b.total().cmp(&a.total()).then_with(|| a_id.cmp(b_id)));

    let mut stderr = std::io::stderr();
    writeln!(stderr, "\nprofile — where the run spent its time\n")?;
    writeln!(
        stderr,
        "  {:<40} {:>9} {:>9} {:>9} {:>9}",
        "rule", "query", "handler", "total", "matches"
    )?;

    for (id, timing) in ranked {
        writeln!(
            stderr,
            "  {:<40} {:>9.1?} {:>9.1?} {:>9.1?} {:>9}",
            id.to_string(),
            timing.query,
            timing.handler,
            timing.total(),
            timing.matches
        )?;
    }

    // The split is the whole point, so say what it means rather than leaving two columns to
    // be interpreted.
    writeln!(
        stderr,
        "\n  query time is a rule matching more than it needs — narrow the query\n  \
         handler time is the rule's own code\n"
    )?;
    stderr.flush()?;
    Ok(())
}

/// The config a fresh project starts with.
///
/// A built-in rule and a local one, because the second is the thing worth showing: lanekeep
/// exists for conventions a model cannot infer, and every one of those is project-authored.
/// A starter that only configured built-ins would teach the wrong lesson about what the tool
/// is for.
const STARTER_CONFIG: &str = r"import { defineConfig } from 'lanekeep'
import noDefaultExport from 'lanekeep/no-default-export'

import noDebugger from './lanekeep/rules/no-debugger'

export default defineConfig({
  include: ['src/**/*.{ts,tsx}'],
  exclude: ['**/*.{test,spec}.{ts,tsx}'],

  rules: [noDefaultExport, noDebugger],
})
";

/// The rule a fresh project starts with.
///
/// Deliberately trivial, and deliberately complete: a card with both examples, a gate, a
/// query and a handler. Someone editing this to write their first real rule should be
/// changing parts, not discovering which parts exist.
const STARTER_RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-debugger',
  severity: 'error',

  // The card is not documentation. It is fed back to whoever has to act on the
  // violation — increasingly an agent — so `remediation` is the field worth the effort.
  card: {
    message: 'debugger statement',
    remediation: 'remove it before committing',
    examples: {
      bad: 'function pay() { debugger; }',
      good: 'function pay() { /* ... */ }',
    },
  },

  // Cheap and exact: a file whose bytes never contain `debugger` is never parsed.
  gates: {
    fileContains: ['debugger'],
  },

  // The query is the gate that matters. Rust matches it; only matches reach `check`,
  // which is what keeps a JavaScript rule affordable.
  query: '(debugger_statement) @stmt',

  check(ctx, m) {
    ctx.report(m.stmt)
  },
})
";

/// Write a starter config and rule.
///
/// Refuses to overwrite without `--force`. A config is a file someone has edited by the time
/// they run this again, and silently replacing it would destroy work that nothing else has a
/// copy of.
fn init(project_root: &Path, force: bool) -> anyhow::Result<ExitCode> {
    let config = project_root.join("lanekeep.config.ts");
    let rule = project_root.join("lanekeep/rules/no-debugger.ts");

    let existing: Vec<&Path> = [config.as_path(), rule.as_path()]
        .into_iter()
        .filter(|path| path.exists())
        .collect();

    if !existing.is_empty() && !force {
        anyhow::bail!(
            "{} already exist(s):\n{}\n  pass --force to overwrite",
            if existing.len() == 1 {
                "a file"
            } else {
                "files"
            },
            existing
                .iter()
                .map(|p| format!("    {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    for (path, contents) in [(&config, STARTER_CONFIG), (&rule, STARTER_RULE)] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::write(path, contents)
            .map_err(|e| anyhow::anyhow!("cannot write `{}`: {e}", path.display()))?;
    }

    let mut stdout = std::io::stdout();
    writeln!(stdout, "created {}", config.display())?;
    writeln!(stdout, "created {}", rule.display())?;
    writeln!(
        stdout,
        "\nrun `lanekeep check` to try it, and `lanekeep explain local/no-debugger` to see \
         what a rule card looks like"
    )?;
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Resolve the config path, defaulting to the first candidate that exists.
fn config_path(project_root: &Path, given: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = given {
        anyhow::ensure!(path.exists(), "no config at `{}`", path.display());
        return Ok(path.to_path_buf());
    }

    let candidates = lanekeep_config::default_config_paths(project_root);
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no config found in `{}`\n  looked for: {}\n  \
                 create one exporting `defineConfig({{ rules: [] }})`",
                project_root.display(),
                candidates
                    .iter()
                    .filter_map(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        })
}

/// Load the config and prepare an engine. Shared by every command that needs rules.
fn prepare(
    project_root: &Path,
    config: Option<&Path>,
    caching: bool,
) -> anyhow::Result<(Engine, usize)> {
    let root = RuleRoot::new(project_root)
        .map_err(|e| anyhow::anyhow!("cannot use `{}`: {e}", project_root.display()))?
        .with_builtins(lanekeep_rules::source);
    let config_path = config_path(project_root, config)?;

    let sandbox = lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let loaded =
        lanekeep_config::load(&sandbox, &root, &config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let declared = loaded.rules.len();

    let engine = Engine::prepare(
        &loaded,
        project_root,
        root,
        &config_path,
        &lanekeep_lang_js::registry(),
        Arc::new(TypeScript),
        Arc::new(JavaScript),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let engine = if caching {
        engine
    } else {
        engine.without_cache()
    };

    Ok((engine, declared))
}

/// Everything `check` was asked for.
///
/// A struct rather than eight parameters: the flags are all independent booleans and paths,
/// which is exactly the shape that gets silently transposed at a call site.
struct CheckOptions<'a> {
    project_root: &'a Path,
    config: Option<&'a Path>,
    format: &'a str,
    timeout: Option<u64>,
    selection: Selection,
    /// The independent on/off switches, grouped rather than passed loose.
    ///
    /// Four bare booleans in a row is the shape that gets silently transposed, and the
    /// compiler cannot help — every one of them is the same type.
    switches: Switches,
}

/// `check`'s boolean flags.
///
/// Four of them, which the lint below normally reads as a type that should have been an
/// enum or a state machine. It is neither: these are independent user-facing switches, and
/// every combination is meaningful. `--fix --warn-only --no-cache` is a coherent request.
#[expect(
    clippy::struct_excessive_bools,
    reason = "a bag of independent CLI flags is where booleans are the right \
              representation — the lint is aimed at domain types, where a pile of them \
              usually means a missing enum"
)]
#[derive(Debug, Clone, Copy)]
struct Switches {
    warn_only: bool,
    no_cache: bool,
    report_unused_suppressions: bool,
    fix: bool,
    profile: bool,
}

fn check(options: CheckOptions<'_>) -> anyhow::Result<ExitCode> {
    let CheckOptions {
        project_root,
        config,
        format,
        timeout,
        selection,
        switches:
            Switches {
                warn_only,
                no_cache,
                report_unused_suppressions,
                fix,
                profile,
            },
    } = options;

    let format = Format::parse(format).map_err(|got| {
        anyhow::anyhow!("unknown --format `{got}`\n  expected: human, json, sarif, agent")
    })?;

    // Rejected rather than accepted and ignored. A user passing `--timeout 0` has a
    // reason, and silently not applying it would surface much later as an unexplained
    // breach of a budget they thought they had changed.
    anyhow::ensure!(
        timeout.is_none_or(|ms| ms > 0),
        "--timeout must be greater than zero"
    );

    let (engine, _) = prepare(project_root, config, !no_cache)?;

    let selected = selection.resolve(project_root)?;
    let cross_file: Vec<String> = engine
        .rules()
        .filter(|spec| spec.has_reduce)
        .map(|spec| spec.id.to_string())
        .collect();

    // A cross-file rule consumes facts from every file, so running one over a subset does
    // not give a smaller answer — it gives a wrong one. Skipping is the only sound option,
    // and it has to be said out loud: a rule that quietly stops running is worse than one
    // that never ran, because the clean output reads as "fixed".
    let engine = if profile { engine.profiling() } else { engine };

    let engine = if report_unused_suppressions {
        engine.reporting_unused_suppressions()
    } else {
        engine
    };

    let engine = if selection.is_narrowed() && !cross_file.is_empty() {
        let mut stderr = std::io::stderr();
        writeln!(
            stderr,
            "note: {} does not run cross-file rules, because they need the whole corpus\n               not run: {}\n               run `lanekeep check` with no file selection to include them",
            selection.flag(),
            cross_file.join(", "),
        )?;
        engine.without_reduce()
    } else {
        engine
    };

    let outcome = match selected {
        // Intersected with discovery rather than used directly, so `include` and `exclude`
        // stay in force — `--staged` must not check a file the config excluded.
        Some(selected) => {
            let wanted: std::collections::BTreeSet<&FilePath> = selected.iter().collect();
            let files: Vec<FilePath> = engine
                .discover()
                .into_iter()
                .filter(|file| wanted.contains(file))
                .collect();
            engine.run_over(&files)
        }
        None => engine.run(),
    }
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let outcome = if fix {
        fix_and_recheck(project_root, config, !no_cache, outcome)?
    } else {
        outcome
    };

    let color = Color::resolve(
        std::io::stdout().is_terminal(),
        std::env::var("NO_COLOR").ok().as_deref(),
    );
    // Cards for every configured rule. The agent and SARIF reporters describe a rule as
    // well as its violations, and a `Violation` carries the message and remediation but not
    // the examples.
    let cards: lanekeep_report::Cards = engine
        .rules()
        .map(|spec| (spec.id.clone(), spec.card.clone()))
        .collect();

    let rendered = lanekeep_report::render(
        format,
        color,
        &outcome.violations,
        Summary {
            files_discovered: outcome.files_discovered,
            files_parsed: outcome.files_parsed,
            warn_only,
        },
        &cards,
    );

    let mut stdout = std::io::stdout();
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()?;

    if let Some(timings) = &outcome.timings {
        // To stderr, so `--profile --format json` still pipes a clean document.
        write_profile(timings)?;
    }

    let code = lanekeep_report::exit_code(&outcome.violations, warn_only);
    Ok(ExitCode::from(
        u8::try_from(code).unwrap_or(EXIT_RUNTIME_ERROR),
    ))
}

fn rules(project_root: &Path, config: Option<&Path>, as_json: bool) -> anyhow::Result<ExitCode> {
    let (engine, declared) = prepare(project_root, config, false)?;
    let mut stdout = std::io::stdout();

    if as_json {
        let listing: Vec<serde_json::Value> = engine.rules().map(rule_json).collect();
        let document = serde_json::json!({
            "version": 1,
            "declared": declared,
            "enabled": engine.rule_count(),
            "rules": listing,
        });
        writeln!(stdout, "{}", serde_json::to_string_pretty(&document)?)?;
        stdout.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    for spec in engine.rules() {
        // Id, severity, then what the rule is for. An agent reading this has to be able to
        // decide whether a rule is the one it wants without opening the source.
        writeln!(stdout, "{}  [{}]", spec.id, spec.severity)?;
        writeln!(stdout, "    {}", spec.card.message)?;
        writeln!(stdout, "    {}", spec.card.remediation)?;
    }

    let disabled = declared.saturating_sub(engine.rule_count());
    if disabled > 0 {
        // Named separately rather than folded into the count, so "I configured six rules
        // and five ran" is visible instead of inferable.
        writeln!(stdout, "\n{declared} rule(s) configured, {disabled} off")?;
    } else {
        writeln!(stdout, "\n{declared} rule(s) configured")?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// One rule as JSON, shared by `rules --json` and `explain --json`.
fn rule_json(spec: &lanekeep_config::RuleSpec) -> serde_json::Value {
    serde_json::json!({
        "id": spec.id.to_string(),
        "severity": spec.severity.to_string(),
        "language": spec.language.clone(),
        "message": spec.card.message.clone(),
        "remediation": spec.card.remediation.clone(),
        "examples": {
            "bad": spec.card.examples.bad.clone(),
            "good": spec.card.examples.good.clone(),
        },
        "query": spec.query.clone(),
        "crossFile": spec.has_reduce,
    })
}

/// Print one rule's card.
///
/// The card exists to be read by whoever has to act on a violation, which is increasingly an
/// agent rather than a person — so this is the command that turns a rule id in a diagnostic
/// into something actionable without opening the rule's source.
fn explain(
    rule: &str,
    project_root: &Path,
    config: Option<&Path>,
    as_json: bool,
) -> anyhow::Result<ExitCode> {
    let (engine, _) = prepare(project_root, config, false)?;

    let Some(spec) = engine.rules().find(|spec| spec.id.to_string() == rule) else {
        // Naming what is configured, rather than only what is missing. A rule id is easy to
        // mistype and the answer is always in the list, so printing the list is more useful
        // than any guess at what was meant.
        let configured: Vec<String> = engine.rules().map(|spec| spec.id.to_string()).collect();
        let near: Vec<&String> = configured
            .iter()
            .filter(|id| {
                // A suffix match catches the common mistake by a wide margin: writing
                // `no-default-export` for `lanekeep/no-default-export`.
                id.ends_with(rule) || id.contains(rule) || rule.contains(id.as_str())
            })
            .collect();

        anyhow::bail!(
            "no rule `{rule}` is configured{}\n  configured: {}",
            if near.is_empty() {
                String::new()
            } else {
                format!(
                    "\n  did you mean: {}",
                    near.iter()
                        .map(|id| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            if configured.is_empty() {
                "none".to_owned()
            } else {
                configured.join(", ")
            },
        );
    };

    let mut stdout = std::io::stdout();

    if as_json {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&rule_json(spec))?
        )?;
        stdout.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    writeln!(stdout, "{}  [{}]", spec.id, spec.severity)?;
    writeln!(stdout, "\n{}", spec.card.message)?;
    writeln!(stdout, "\nFix: {}", spec.card.remediation)?;
    writeln!(stdout, "\nBad:  {}", spec.card.examples.bad)?;
    writeln!(stdout, "Good: {}", spec.card.examples.good)?;

    if spec.has_reduce {
        // Worth stating: it changes what `--since` and `--staged` do with the rule.
        writeln!(
            stdout,
            "\nThis rule reads the whole corpus, so --since and --staged skip it."
        )?;
    }

    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}
