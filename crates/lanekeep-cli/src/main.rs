//! Command-line interface for lanekeep.
//!
//! See `docs/architecture.md` §12 for the command surface.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

mod watch;

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

        /// Recheck every file, ignoring and not writing the result cache.
        ///
        /// For diagnosing a suspected stale result. If one is ever found this way, the
        /// cache key is missing an input — that is a bug, not a reason to keep the flag on.
        ///
        /// It governs the result cache and nothing else. A run still writes precompiled
        /// components into `.lanekeep/components`, which for a project naming one of the
        /// TypeScript built-ins is about 33 MiB — worth knowing before running this in a
        /// container you expected to leave clean. That is deliberate: an artifact is named by
        /// a hash of the component's own bytes, so there is no stale one to serve and nothing
        /// this flag exists to diagnose can come from it, while suppressing it would add
        /// several seconds of recompilation to every diagnostic run. `ComponentLoader`'s own
        /// documentation records the same decision from the other side.
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

        /// Re-check whenever the project changes, until interrupted.
        ///
        /// A foreground loop, not a daemon: it holds nothing a fresh run would not rebuild,
        /// and Ctrl-C ends it. The warm cache is what makes each re-run fast.
        #[arg(long, conflicts_with = "fix")]
        watch: bool,
    },

    /// Serve diagnostics to an editor or an agent host, over stdio.
    ///
    /// JSON-RPC 2.0 on stdin and stdout. Nothing is printed to stdout that is not a
    /// protocol message — a stray line there desynchronizes the client for good.
    Server {
        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Config file.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Which protocol to speak: `lsp` for an editor, `mcp` for an agent host.
        #[arg(long, default_value = "lsp")]
        protocol: String,
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
            watch,
        } => {
            let options = || CheckOptions {
                project_root: &path,
                config: config.as_deref(),
                format: &format,
                timeout,
                selection: Selection::from(since.clone(), staged),
                switches: Switches {
                    warn_only,
                    no_cache,
                    report_unused_suppressions,
                    fix,
                    profile,
                },
            };

            if watch {
                // The exit code of any single pass is not the loop's: a violation is
                // something to show and wait past, not a reason to stop watching. What the
                // loop reports is whether it could watch at all.
                return watch::watch(&path, || check(options()).map(|_| ()));
            }

            check(CheckOptions {
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
            })
        }
        Command::Server {
            path,
            config,
            protocol,
        } => server(&path, config.as_deref(), &protocol),
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
    global_timeout: Option<u64>,
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

    let (engine, _) = prepare(project_root, config, caching, global_timeout)?;
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

/// Print what each rule's gates let through, per rule.
///
/// A second table rather than four more columns on [`write_profile`]'s: that one answers "which
/// rule is expensive", so it sorts by total elapsed time; this one answers "what did each rule
/// even look at", which is not a time question, so sorting it by elapsed time would reorder rows
/// for a reason unrelated to what they show. Sorted by rule id instead, which is stable across
/// runs over the same corpus.
///
/// `files_discovered` is threaded through rather than derived from a row's own sum, on purpose:
/// the trailing line is the reader's reconciliation check, and computing it from the same
/// numbers it is meant to check would make a miscount invisible instead of visible.
fn write_gate_profile(
    timings: &BTreeMap<lanekeep_core::RuleId, lanekeep_engine::RuleTiming>,
    files_discovered: usize,
) -> anyhow::Result<()> {
    let mut ranked: Vec<_> = timings.iter().collect();
    ranked.sort_by_key(|(id, _)| (*id).clone());

    let mut stderr = std::io::stderr();
    writeln!(stderr, "\nprofile — what each rule looked at\n")?;
    writeln!(
        stderr,
        "  {:<40} {:>10} {:>6} {:>6} {:>13} {:>10} {:>6}",
        "rule", "path-gated", "unread", "cached", "content-gated", "lang-gated", "parsed"
    )?;

    for (id, timing) in ranked {
        writeln!(
            stderr,
            "  {:<40} {:>10} {:>6} {:>6} {:>13} {:>10} {:>6}",
            id.to_string(),
            timing.path_gated,
            timing.unread,
            timing.cached,
            timing.content_gated,
            timing.language_gated,
            timing.parsed
        )?;
    }

    // Each row's six counters reconcile to this figure — see `RuleTiming`'s doc in
    // `lanekeep-engine` for why. A rule silent behind a large `content-gated` is a gate
    // question; one silent behind a large `lang-gated` is a `language` declaration that does
    // not name the grammar its files actually parse with — the failure `AGENTS.md` records as
    // 2218 false positives in the mirror direction, arriving here as the opposite symptom.
    writeln!(
        stderr,
        "\n  each row sums to {files_discovered} files discovered\n"
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
/// What `init` writes, chosen by what the project looks like.
///
/// A Go team running `lanekeep init` and receiving a TypeScript config with a rule about
/// `debugger` statements learns that this tool is not really for them. The config format is
/// only half of that; the other half is scaffolding a rule and an include glob that match
/// the code actually in the repository.
///
/// Rules stay TypeScript in every case — that is ADR-0007 and it does not bend. What varies
/// is which rule, which glob, and which built-ins are worth turning on.
struct Scaffold {
    /// What was detected, for the message printed at the end.
    language: &'static str,
    /// The `include` glob.
    include: &'static str,
    /// The `exclude` glob, or none when the language has no obvious test convention.
    exclude: Option<&'static str>,
    /// Built-in rules to enable, each pre-formatted as a `rules` array entry — a quoted bare
    /// specifier, or the `{ "rule": ..., "options": {} }` object form a factory rule needs.
    ///
    /// `lanekeep/no-unwrap` is a factory — it is how its `allow` option is reached at all —
    /// so a bare quoted string for it would fail config load with ``missing `id` `` the moment a
    /// Rust project ran `init`. Formatting each entry here rather than quoting a bare
    /// specifier in `starter_config` is what lets the two forms coexist without
    /// `starter_config` having to know which built-ins are factories.
    builtins: &'static [&'static str],
    /// Filename of the starter rule, under `lanekeep/rules/`.
    rule_file: &'static str,
    /// Its source.
    rule: &'static str,
}

const TYPESCRIPT_RULE: &str = r"import { defineRule } from 'lanekeep'

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

const PYTHON_RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-print',
  language: 'python',
  severity: 'error',

  // The card is not documentation. It is fed back to whoever has to act on the
  // violation — increasingly an agent — so `remediation` is the field worth the effort.
  card: {
    message: 'print() in library code',
    remediation: 'use the logging module, so the output has a level and a destination',
    examples: {
      bad: 'print(f\'saved {count}\')',
      good: 'logger.info(\'saved %s\', count)',
    },
  },

  // Cheap and exact: a file whose bytes never contain `print` is never parsed.
  gates: {
    fileContains: ['print'],
  },

  // The query is the gate that matters. Rust matches it; only matches reach `check`.
  query: '(call function: (identifier) @name) @call',

  check(ctx, m) {
    if (ctx.text(m.name) !== 'print') return

    // A project that defined its own `print` is not calling the builtin.
    if (ctx.bindingKind(m.name)) return

    ctx.report(m.call)
  },
})
";

const GO_RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-fmt-println',
  language: 'go',
  severity: 'error',

  // The card is not documentation. It is fed back to whoever has to act on the
  // violation — increasingly an agent — so `remediation` is the field worth the effort.
  card: {
    message: 'fmt.Println in library code',
    remediation: 'use log/slog, so the output has a level and a destination',
    examples: {
      bad: 'fmt.Println(\'saved\', count)',
      good: 'slog.Info(\'saved\', \'count\', count)',
    },
  },

  // Cheap and exact: a file whose bytes never contain `fmt` is never parsed.
  gates: {
    fileContains: ['fmt'],
  },

  // The query is the gate that matters. Rust matches it; only matches reach `check`.
  query: `
    (call_expression
      function: (selector_expression
        operand: (identifier) @pkg
        field: (field_identifier) @fn)) @call
  `,

  check(ctx, m) {
    if (ctx.text(m.pkg) !== 'fmt') return
    if (ctx.text(m.fn) !== 'Println' && ctx.text(m.fn) !== 'Printf') return

    // A local variable named `fmt` is not the standard library package.
    if (ctx.bindingKind(m.pkg) !== 'import') return

    ctx.report(m.call)
  },
})
";

const RUST_RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-dbg',
  language: 'rust',
  severity: 'error',

  // The card is not documentation. It is fed back to whoever has to act on the
  // violation — increasingly an agent — so `remediation` is the field worth the effort.
  card: {
    message: 'dbg! left in the source',
    remediation: 'remove it, or use tracing if the output is meant to stay',
    examples: {
      bad: 'let total = dbg!(sum(&items));',
      good: 'let total = sum(&items);',
    },
  },

  // Cheap and exact: a file whose bytes never contain `dbg!` is never parsed.
  gates: {
    fileContains: ['dbg!'],
  },

  // The query is the gate that matters. Rust matches it; only matches reach `check`.
  query: '(macro_invocation macro: (identifier) @name) @call',

  check(ctx, m) {
    if (ctx.text(m.name) !== 'dbg') return
    ctx.report(m.call)
  },
})
";

const TYPESCRIPT: Scaffold = Scaffold {
    language: "TypeScript",
    include: "src/**/*.{ts,tsx}",
    exclude: Some("**/*.{test,spec}.{ts,tsx}"),
    builtins: &["\"lanekeep/no-default-export\""],
    rule_file: "no-debugger.ts",
    rule: TYPESCRIPT_RULE,
};

const PYTHON: Scaffold = Scaffold {
    language: "Python",
    include: "**/*.py",
    exclude: Some("**/test_*.py"),
    builtins: &["\"lanekeep/no-broad-except\""],
    rule_file: "no-print.ts",
    rule: PYTHON_RULE,
};

const GO: Scaffold = Scaffold {
    language: "Go",
    include: "**/*.go",
    exclude: Some("**/*_test.go"),
    builtins: &["\"lanekeep/no-package-init\""],
    rule_file: "no-fmt-println.ts",
    rule: GO_RULE,
};

const RUST: Scaffold = Scaffold {
    language: "Rust",
    include: "src/**/*.rs",
    // A Rust project keeps its unit tests beside the code they cover, so there is no test
    // path to exclude — `#[cfg(test)]` is the convention, not a directory.
    exclude: None,
    // The object form, not a bare specifier: `no-unwrap` is a factory, and calling it with no
    // options is what makes it behave the way the bare form used to.
    builtins: &["{ \"rule\": \"lanekeep/no-unwrap\", \"options\": {} }"],
    rule_file: "no-dbg.ts",
    rule: RUST_RULE,
};

/// What this project looks like, from the manifest a language cannot really do without.
///
/// Checked in a fixed order so a polyglot repository scaffolds predictably rather than
/// according to directory iteration. TypeScript is last because a Go or Python service with
/// a small web frontend is more usefully scaffolded for its backend, and because that is the
/// one a user is least surprised to have to change.
fn detect(project_root: &Path) -> &'static Scaffold {
    for (marker, scaffold) in [
        ("Cargo.toml", &RUST),
        ("go.mod", &GO),
        ("pyproject.toml", &PYTHON),
        ("setup.py", &PYTHON),
        ("requirements.txt", &PYTHON),
        ("tsconfig.json", &TYPESCRIPT),
        ("package.json", &TYPESCRIPT),
    ] {
        if project_root.join(marker).exists() {
            return scaffold;
        }
    }
    &TYPESCRIPT
}

/// The `lanekeep.json` for a scaffold.
///
/// JSON rather than TypeScript because configuration is not a rule. Requiring a Go or Python
/// team to write a `.ts` file to say which rules they want was a coupling with nothing behind
/// it — `lanekeep.config.ts` still works, and is the better choice for a config that computes
/// something or shares a preset.
fn starter_config(scaffold: &Scaffold) -> String {
    // Each entry already carries its own quoting — a bare specifier or an object — so this
    // only has to indent it, not decide which form it needs.
    let mut rules: Vec<String> = scaffold
        .builtins
        .iter()
        .map(|entry| format!("    {entry}"))
        .collect();
    rules.push(format!("    \"./lanekeep/rules/{}\"", scaffold.rule_file));

    let exclude = scaffold.exclude.map_or_else(
        || "  \"exclude\": [],\n".to_owned(),
        |glob| format!("  \"exclude\": [\"{glob}\"],\n"),
    );

    format!(
        "{{\n  \"$schema\": \"{SCHEMA_URL}\",\n\n  \"include\": [\"{}\"],\n{exclude}\n  \"rules\": [\n{}\n  ]\n}}\n",
        scaffold.include,
        rules.join(",\n"),
    )
}

/// Where editors fetch the config schema from.
///
/// Pinned to `main` rather than to a tag: a schema describing fields a user's version does
/// not have yet is a worse failure than one describing all of them, since the first shows up
/// as an editor warning on correct config.
const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/fmsouza/lanekeep/main/schema/lanekeep.schema.json";

/// Write a starter config and rule.
///
/// Refuses to overwrite without `--force`. A config is a file someone has edited by the time
/// they run this again, and silently replacing it would destroy work that nothing else has a
/// copy of.
fn init(project_root: &Path, force: bool) -> anyhow::Result<ExitCode> {
    let scaffold = detect(project_root);

    let config = project_root.join("lanekeep.json");
    let rule = project_root.join(format!("lanekeep/rules/{}", scaffold.rule_file));

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

    let config_source = starter_config(scaffold);
    for (path, contents) in [(&config, config_source.as_str()), (&rule, scaffold.rule)] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::write(path, contents)
            .map_err(|e| anyhow::anyhow!("cannot write `{}`: {e}", path.display()))?;
    }

    let ignored = ignore_the_cache(project_root)?;

    let mut stdout = std::io::stdout();
    writeln!(stdout, "detected a {} project", scaffold.language)?;
    writeln!(stdout, "created {}", config.display())?;
    writeln!(stdout, "created {}", rule.display())?;
    if ignored {
        writeln!(stdout, "added .lanekeep/ to .gitignore")?;
    }
    // The rule id is derived from the filename rather than written down twice, so the
    // command printed here cannot drift from the rule that was actually written.
    let starter_id = scaffold.rule_file.trim_end_matches(".ts");
    writeln!(
        stdout,
        "\nrun `lanekeep check` to try it, and `lanekeep explain local/{starter_id}` to see \
         what a rule card looks like"
    )?;
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Add `.lanekeep/` to the project's `.gitignore`, if it is not already covered.
///
/// The cache is a multi-megabyte binary that the first `lanekeep check` drops into the
/// working tree. Left untracked and unmentioned, it gets committed — which is a large,
/// meaningless diff, and machine-specific content in the repository.
///
/// Appended rather than created from scratch when a `.gitignore` already exists, and never
/// written at all outside a git repository, where the file would be noise.
fn ignore_the_cache(project_root: &Path) -> anyhow::Result<bool> {
    const ENTRY: &str = ".lanekeep/";

    if !project_root.join(".git").exists() {
        return Ok(false);
    }

    let path = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Matched on whole lines: a `.gitignore` mentioning `.lanekeep/cache` covers a different
    // thing, and appending a second entry that covers the first is only confusing.
    if existing
        .lines()
        .any(|line| line.trim() == ENTRY || line.trim() == ".lanekeep")
    {
        return Ok(false);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str("# lanekeep's content-addressed cache\n");
    updated.push_str(ENTRY);
    updated.push('\n');

    std::fs::write(&path, updated)
        .map_err(|e| anyhow::anyhow!("cannot write `{}`: {e}", path.display()))?;
    Ok(true)
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
    global_timeout: Option<u64>,
) -> anyhow::Result<(Engine, usize)> {
    let root = RuleRoot::new(project_root)
        .map_err(|e| anyhow::anyhow!("cannot use `{}`: {e}", project_root.display()))?
        .with_builtins(lanekeep_rules::source)
        // The two halves of one table. A built-in is a module or a component depending only on
        // how it happens to be authored in this build, and a config writes `lanekeep/<name>`
        // either way — so both lookups have to be installed together, or a rule that migrated
        // stops resolving for everyone who never changed anything.
        .with_builtin_components(lanekeep_rules::component)
        // And the third: the source maps of the components that have one, so a built-in that
        // throws is reported at a line in the TypeScript it was authored in rather than at one
        // in the bundle it was compiled into. Only a diagnostic depends on this — a rule that
        // resolves without it works identically and reports the same violations — which is why
        // it is asserted end to end in `crates/lanekeep-rules/tests/source_maps.rs` rather than
        // left to whoever notices.
        .with_builtin_component_maps(lanekeep_rules::component_source_map)
        // And the fourth: the "declared as a component" table, so a name whose component row is
        // broken (its host missing from the table) is refused as a lanekeep bug rather than
        // served from a stale TypeScript source or reported as a misspelling.
        .with_builtin_component_declared(lanekeep_rules::is_declared_component);
    let config_path = config_path(project_root, config)?;

    let sandbox = lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Both of these have to be known *before* the load, and for the same reason: config load is
    // itself a phase that runs guest code, so anything applied to the `Config` it returns arrives
    // after the phase it was meant to govern.
    //
    // `artifacts` hands config load the same `.lanekeep/components` the engine's own loader uses,
    // so a component is compiled once for the project instead of once per config load and again
    // per prepare. `prepare` runs per LSP request, per MCP tool call and per `--watch` iteration,
    // and without it that was ~58 ms per component on each of them — for a 26 KB Rust component,
    // which is what was measured. The shared TypeScript built-ins are one 12.4 MiB artifact and
    // cost about six seconds to compile, so on a project naming any of those four this line is
    // the difference between an interactive command and an unusable one.
    //
    // `global_timeout` is `--timeout`, which overrides whatever the config settled on because a
    // flag a user typed on this run is a more specific statement than a file that applies to every
    // run. It has now been dropped on the floor twice, in two different phases, which is why it is
    // passed in rather than assigned afterwards. The first time, the flag parsed, `--timeout 0`
    // was rejected with a message, and the budget never moved. The second time — this branch — it
    // moved the *run's* budget from a line below a config load that had already instantiated,
    // configured and read metadata from every component under the config file's number. Both times
    // the breach message ended "raise it with `--timeout`", which was advice that could not work.
    let loaded = lanekeep_config::load_with(
        &sandbox,
        &root,
        &config_path,
        lanekeep_config::LoadOptions {
            artifacts: Some(project_root),
            global_timeout: global_timeout.map(std::time::Duration::from_millis),
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let declared = loaded.rules.len();

    let engine = Engine::prepare(
        &loaded,
        project_root,
        root,
        &config_path,
        &lanekeep_languages::registry(),
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

/// Serve LSP or MCP over stdio until the client disconnects.
///
/// The engine is rebuilt on every call rather than held: a rule file or the config can change
/// while the session is open, and a server answering from the ruleset it started with would
/// report violations the project no longer has.
fn server(project_root: &Path, config: Option<&Path>, protocol: &str) -> anyhow::Result<ExitCode> {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    match protocol {
        "lsp" => {
            lanekeep_server::serve_lsp(&mut input, &mut output, &root, || {
                // Every failure becomes a string the server logs and carries on from. An
                // editor session should survive a config typo, not end on one.
                let (engine, _) =
                    prepare(project_root, config, true, None).map_err(|e| e.to_string())?;
                engine
                    .run()
                    .map(|outcome| outcome.violations)
                    .map_err(|e| e.to_string())
            })?;
        }
        "mcp" => {
            let mut tools = Project {
                project_root,
                config,
            };
            lanekeep_server::mcp::serve(&mut input, &mut output, &mut tools)?;
        }
        other => anyhow::bail!("unknown --protocol `{other}`\n  expected: lsp, mcp"),
    }

    Ok(ExitCode::SUCCESS)
}

/// The MCP tools, run against a real project.
struct Project<'a> {
    project_root: &'a Path,
    config: Option<&'a Path>,
}

impl lanekeep_server::mcp::Tools for Project<'_> {
    fn check(&mut self) -> Result<String, String> {
        let (engine, _) =
            prepare(self.project_root, self.config, true, None).map_err(|e| e.to_string())?;
        let outcome = engine.run().map_err(|e| e.to_string())?;

        let cards: lanekeep_report::Cards = engine
            .rules()
            .map(|spec| (spec.id.clone(), spec.card.clone()))
            .collect();

        // The `agent` format, which exists for exactly this consumer — grouped by rule, with
        // the remediation stated once and an example either way. Never colored: escape codes
        // in a model's context are noise it pays for.
        Ok(lanekeep_report::render(
            Format::Agent,
            Color::Never,
            &outcome.violations,
            Summary {
                files_discovered: outcome.files_discovered,
                files_parsed: outcome.files_parsed,
                warn_only: false,
            },
            &cards,
        ))
    }

    fn rules(&mut self) -> Result<String, String> {
        use std::fmt::Write as _;

        let (engine, _) =
            prepare(self.project_root, self.config, false, None).map_err(|e| e.to_string())?;

        let mut out = String::new();
        for spec in engine.rules() {
            let _ = writeln!(
                out,
                "{} [{}] — {}",
                spec.id, spec.severity, spec.card.message
            );
        }
        if out.is_empty() {
            out.push_str("no rules are configured\n");
        }
        Ok(out)
    }

    fn explain(&mut self, rule: &str) -> Result<String, String> {
        use std::fmt::Write as _;

        let (engine, _) =
            prepare(self.project_root, self.config, false, None).map_err(|e| e.to_string())?;

        let Some(spec) = engine.rules().find(|spec| spec.id.to_string() == rule) else {
            // The list, not only the miss. A rule id is easy to mistype and the answer is
            // always in the list, which is more use to a model than any guess at the intent.
            let configured: Vec<String> = engine.rules().map(|spec| spec.id.to_string()).collect();
            return Err(format!(
                "no rule `{rule}` is configured\n  configured: {}",
                if configured.is_empty() {
                    "none".to_owned()
                } else {
                    configured.join(", ")
                }
            ));
        };

        let mut out = String::new();
        let _ = writeln!(out, "{} [{}]", spec.id, spec.severity);
        let _ = writeln!(out, "{}", spec.card.message);
        let _ = writeln!(out, "Fix: {}", spec.card.remediation);
        let _ = writeln!(out, "Bad:  {}", spec.card.examples.bad);
        let _ = writeln!(out, "Good: {}", spec.card.examples.good);
        Ok(out)
    }
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

    let (engine, _) = prepare(project_root, config, !no_cache, timeout)?;

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
        fix_and_recheck(project_root, config, !no_cache, timeout, outcome)?
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
        write_gate_profile(timings, outcome.files_discovered)?;
    }

    let code = lanekeep_report::exit_code(&outcome.violations, warn_only);
    Ok(ExitCode::from(
        u8::try_from(code).unwrap_or(EXIT_RUNTIME_ERROR),
    ))
}

fn rules(project_root: &Path, config: Option<&Path>, as_json: bool) -> anyhow::Result<ExitCode> {
    let (engine, declared) = prepare(project_root, config, false, None)?;
    let mut stdout = std::io::stdout();

    if as_json {
        let listing: Vec<serde_json::Value> = engine.rules().map(rule_json).collect();
        // Version 2: a rule's singular `query` string became a `queries` object keyed by
        // language. The document is consumed by machines, so a shape change moves the
        // version even when every other field is unchanged.
        let document = serde_json::json!({
            "version": 2,
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
///
/// `queries` — plural, an object keyed by language — replaced a singular `query` string
/// when rules grew one query per language. The key changed with the shape on purpose: a
/// consumer written against the string form fails loudly on a missing key instead of
/// receiving an object where it read a string, and the `rules --json` envelope's `version`
/// moved with it.
fn rule_json(spec: &lanekeep_config::RuleSpec) -> serde_json::Value {
    serde_json::json!({
        "id": spec.id.to_string(),
        "severity": spec.severity.to_string(),
        "languages": spec.languages.clone(),
        "message": spec.card.message.clone(),
        "remediation": spec.card.remediation.clone(),
        "examples": {
            "bad": spec.card.examples.bad.clone(),
            "good": spec.card.examples.good.clone(),
        },
        "queries": spec.queries.clone(),
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
    let (engine, _) = prepare(project_root, config, false, None)?;

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
