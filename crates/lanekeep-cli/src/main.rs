//! Command-line interface for lanekeep.
//!
//! See `docs/architecture.md` §12 for the command surface.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use lanekeep_core::FilePath;
use lanekeep_engine::Engine;
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
    },

    /// List the rules a project has configured.
    Rules {
        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Config file.
        #[arg(long)]
        config: Option<PathBuf>,
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
        } => check(
            &path,
            config.as_deref(),
            &format,
            warn_only,
            timeout,
            no_cache,
            &Selection::from(since, staged),
        ),
        Command::Rules { path, config } => rules(&path, config.as_deref()),
    }
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

fn check(
    project_root: &Path,
    config: Option<&Path>,
    format: &str,
    warn_only: bool,
    timeout: Option<u64>,
    no_cache: bool,
    selection: &Selection,
) -> anyhow::Result<ExitCode> {
    let format = Format::parse(format)
        .map_err(|got| anyhow::anyhow!("unknown --format `{got}`\n  expected: human, json"))?;

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

    let color = Color::resolve(
        std::io::stdout().is_terminal(),
        std::env::var("NO_COLOR").ok().as_deref(),
    );
    let rendered = lanekeep_report::render(
        format,
        color,
        &outcome.violations,
        Summary {
            files_discovered: outcome.files_discovered,
            files_parsed: outcome.files_parsed,
            warn_only,
        },
    );

    let mut stdout = std::io::stdout();
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()?;

    let code = lanekeep_report::exit_code(&outcome.violations, warn_only);
    Ok(ExitCode::from(
        u8::try_from(code).unwrap_or(EXIT_RUNTIME_ERROR),
    ))
}

fn rules(project_root: &Path, config: Option<&Path>) -> anyhow::Result<ExitCode> {
    let (engine, declared) = prepare(project_root, config, false)?;

    let mut stdout = std::io::stdout();
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
