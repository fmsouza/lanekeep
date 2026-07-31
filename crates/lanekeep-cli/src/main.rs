//! Command-line interface for lanekeep.
//!
//! See `docs/architecture.md` §12 for the command surface.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
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
        } => check(&path, config.as_deref(), &format, warn_only, timeout),
        Command::Rules { path, config } => rules(&path, config.as_deref()),
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
fn prepare(project_root: &Path, config: Option<&Path>) -> anyhow::Result<(Engine, usize)> {
    let root = RuleRoot::new(project_root)
        .map_err(|e| anyhow::anyhow!("cannot use `{}`: {e}", project_root.display()))?;
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

    Ok((engine, declared))
}

fn check(
    project_root: &Path,
    config: Option<&Path>,
    format: &str,
    warn_only: bool,
    timeout: Option<u64>,
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

    let (engine, _) = prepare(project_root, config)?;
    let outcome = engine.run().map_err(|e| anyhow::anyhow!("{e}"))?;

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
    let (engine, declared) = prepare(project_root, config)?;

    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{declared} rule(s) configured, {} enabled",
        engine.rule_count()
    )?;
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}
