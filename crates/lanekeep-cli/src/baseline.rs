//! `--baseline` and `--write-baseline`: the file, the source lines, and what is said about them.
//!
//! The matching itself is [`lanekeep_core::baseline`], which does no IO. This module reads and
//! writes the JSON, reads the lines a fingerprint is taken over, and prints the two notes a
//! baselined run owes its reader — how much it hid, and which entries no longer occur. Both go
//! to stderr, so `--format json` still pipes a clean document.
//!
//! A baseline filters an outcome after `Engine::run` returns. Nothing here reaches the engine,
//! the sandbox or the cache, which is why a cached result is the same with or without one.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use lanekeep_core::baseline::{self, Applied, Baseline, Scope};
use lanekeep_core::{FilePath, Violation};

/// What a run was asked to do with a baseline.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Mode<'a> {
    /// Nothing: report every violation.
    Off,
    /// Report only violations this file does not record.
    Compare(&'a Path),
    /// Record every violation to this file, then report what it could not record.
    Write(&'a Path),
}

impl<'a> Mode<'a> {
    /// From the two flags, which clap already refuses together.
    pub(crate) fn from(compare: Option<&'a Path>, write: Option<&'a Path>) -> Self {
        match (compare, write) {
            (_, Some(path)) => Self::Write(path),
            (Some(path), None) => Self::Compare(path),
            (None, None) => Self::Off,
        }
    }
}

/// Apply `mode` to a run's violations and return the ones to report.
///
/// # Errors
///
/// Fails if the baseline cannot be read, is not a baseline, is another version, or cannot be
/// written. Each is a runtime error rather than an empty baseline: a typo silently comparing
/// against nothing would report the whole backlog and name no cause.
pub(crate) fn apply(
    mode: Mode<'_>,
    project_root: &Path,
    violations: Vec<Violation>,
    scope: &Scope,
    out: &mut impl Write,
) -> anyhow::Result<Vec<Violation>> {
    let mut lines = Lines::new(project_root);
    match mode {
        Mode::Off => Ok(violations),
        Mode::Write(path) => {
            let recorded = Baseline::record(&violations, |file, n| lines.line(file, n));
            write(path, &recorded)?;
            let total: u64 = recorded.entries.iter().map(|e| u64::from(e.count)).sum();
            writeln!(
                out,
                "wrote {total} violation(s) as {} baseline entr(ies) to `{}`",
                recorded.entries.len(),
                path.display()
            )?;
            // What is left is exactly what a baseline may not record — `lanekeep/suppression`
            // — so a normal adoption exits 0 and an expired directive still fails the run.
            let applied = recorded.apply(violations, |file, n| lines.line(file, n), scope);
            Ok(applied.new)
        }
        Mode::Compare(path) => {
            let accepted = read(path)?;
            let applied = accepted.apply(violations, |file, n| lines.line(file, n), scope);
            note(out, path, &applied)?;
            Ok(applied.new)
        }
    }
}

/// Read a baseline file.
///
/// The version is checked before the shape, so a file from a newer lanekeep is refused by
/// naming its version rather than by naming whichever of its fields this build does not know.
///
/// # Errors
///
/// Fails if the file cannot be read, is not JSON, is another version, or has an unknown field.
fn read(path: &Path) -> anyhow::Result<Baseline> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("cannot read baseline `{}`: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("baseline `{}` is not JSON: {e}", path.display()))?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(version) if version == u64::from(baseline::VERSION) => {}
        Some(version) => anyhow::bail!(
            "baseline `{}` is version {version}; this lanekeep reads version {}\n  \
             rewrite it with `lanekeep check --write-baseline {}`",
            path.display(),
            baseline::VERSION,
            path.display(),
        ),
        None => anyhow::bail!(
            "baseline `{}` has no `version`; it was not written by `--write-baseline`",
            path.display()
        ),
    }
    serde_json::from_value(value).map_err(|e| {
        anyhow::anyhow!(
            "baseline `{}` is not a lanekeep baseline: {e}",
            path.display()
        )
    })
}

/// Write a baseline: two-space JSON and a trailing newline.
///
/// Entries arrive sorted, so rewriting an unchanged baseline produces identical bytes and a
/// commit shows only what really changed.
///
/// # Errors
///
/// Fails if the file cannot be written.
fn write(path: &Path, baseline: &Baseline) -> anyhow::Result<()> {
    let mut text = serde_json::to_string_pretty(baseline)?;
    text.push('\n');
    std::fs::write(path, text)
        .map_err(|e| anyhow::anyhow!("cannot write baseline `{}`: {e}", path.display()))
}

/// Say what a compared run hid, and which entries no longer occur.
///
/// Neither changes the exit code. Hiding is announced because a run that hides things silently
/// reads as "clean"; stale entries are listed because nothing else will ever say the file can
/// shrink.
fn note(out: &mut impl Write, path: &Path, applied: &Applied) -> std::io::Result<()> {
    if applied.baselined > 0 {
        writeln!(
            out,
            "note: {} violation(s) matched the baseline `{}` and are not shown",
            applied.baselined,
            path.display()
        )?;
    }
    if applied.stale.is_empty() {
        return Ok(());
    }
    let fixed: u64 = applied.stale.iter().map(|e| u64::from(e.count)).sum();
    writeln!(
        out,
        "note: {} baseline entr(ies) no longer occur — {fixed} violation(s) fixed",
        applied.stale.len()
    )?;
    for entry in &applied.stale {
        writeln!(out, "  {} {} ({})", entry.rule, entry.file, entry.count)?;
    }
    writeln!(
        out,
        "  run `lanekeep check --write-baseline {}` to drop them",
        path.display()
    )
}

/// Source lines, read once per file, for fingerprinting.
///
/// Read after the run, the way `--fix` reads. A file that vanished, is not UTF-8, or is
/// shorter than a reported line answers an empty line rather than failing: the fingerprint
/// is then stable for the same absence, and the run is not cancelled over a hygiene feature.
struct Lines {
    root: PathBuf,
    files: BTreeMap<FilePath, Vec<String>>,
}

impl Lines {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: BTreeMap::new(),
        }
    }

    fn line(&mut self, file: &FilePath, n: u32) -> String {
        let root = &self.root;
        let lines = self.files.entry(file.clone()).or_insert_with(|| {
            std::fs::read(root.join(file.as_str()))
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .split('\n')
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        });
        usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| lines.get(index))
            .cloned()
            .unwrap_or_default()
    }
}
