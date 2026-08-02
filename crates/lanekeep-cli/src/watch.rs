//! `--watch`: a foreground loop that re-checks when the project changes.
//!
//! Not a daemon. It runs in the terminal it was started in, holds no state a fresh run would
//! not rebuild, and exits on Ctrl-C. The warm cache is what makes a re-run fast; watching
//! only removes the need to type the command again.
//!
//! # The loop that eats itself
//!
//! lanekeep writes its cache into `.lanekeep/` **inside the project root**, which is also
//! what the watcher watches. A watcher that reacted to every event under the root would see
//! its own cache write, re-check, write the cache again, and never stop — spinning a core at
//! full tilt while appearing to work. [`is_interesting`] is what prevents that, and it is
//! the reason this file has tests at all: the failure is invisible in a screenshot and
//! obvious in a flame graph, which is the wrong way round.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;

/// How long to wait for a burst of events to settle before re-checking.
///
/// A save from an editor is rarely one event: many write a temporary file, rename it over the
/// original, and touch the mode — three events for one logical change. A build tool touching
/// a tree produces hundreds. Without a pause each of those is a separate run, and the runs
/// queue up behind a corpus that has not stopped changing.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Directories whose contents never warrant a re-check.
///
/// `.lanekeep` is the one that matters — see the module docs. The rest are here because a
/// package manager or a build tool writing thousands of files under them would otherwise
/// wake the loop continuously without any of it being source the run would read.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".lanekeep",
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".expo",
];

/// Whether a changed path should wake the loop.
///
/// Deliberately coarse: it excludes what could never be source rather than trying to decide
/// what *is*. Discovery already applies the config's `include` and `exclude`, and duplicating
/// that judgment here would give two answers to one question — the failure mode being a file
/// the run would check that the watcher ignores, which reads as lanekeep missing a violation.
#[must_use]
pub(crate) fn is_interesting(path: &Path) -> bool {
    !path.components().any(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .is_some_and(|name| IGNORED_DIRECTORIES.contains(&name)),
        _ => false,
    })
}

/// Run `check` once, then again whenever the project changes, until interrupted.
///
/// # Errors
///
/// Returns an error if the watcher cannot be created or the root cannot be watched. A failing
/// *check* is not an error here: in a loop, a rule that throws is something to report and go
/// back to waiting for, not a reason to tear the session down.
pub(crate) fn watch(
    root: &Path,
    mut once: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<std::process::ExitCode> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(DEBOUNCE, sender)
        .map_err(|e| anyhow::anyhow!("cannot watch `{}`: {e}", root.display()))?;
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| anyhow::anyhow!("cannot watch `{}`: {e}", root.display()))?;

    report(&mut once);
    let mut stderr = std::io::stderr();
    announce(&mut stderr, root);

    // The receiver ends when the debouncer is dropped, which happens on Ctrl-C taking the
    // process down. There is no other exit: a foreground loop the user started is a loop the
    // user ends.
    while let Ok(event) = receiver.recv() {
        let changed: HashSet<PathBuf> = match event {
            Ok(events) => events
                .into_iter()
                .map(|event| event.path)
                .filter(|path| is_interesting(path))
                .collect(),
            // A watcher error — a directory vanishing mid-scan, a descriptor limit — is
            // reported and waited through. Tearing down the session because one event was
            // lost would be a worse answer than re-checking on the next one.
            Err(error) => {
                let _ = writeln!(std::io::stderr(), "watch: {error}");
                continue;
            }
        };

        if changed.is_empty() {
            continue;
        }

        report(&mut once);
        announce(&mut stderr, root);
    }

    Ok(std::process::ExitCode::SUCCESS)
}

/// Run the check and print what went wrong, without ending the loop.
fn report(once: &mut impl FnMut() -> anyhow::Result<()>) {
    if let Err(error) = once() {
        let _ = writeln!(std::io::stderr(), "lanekeep: {error}");
    }
}

fn announce(out: &mut impl Write, root: &Path) {
    let _ = writeln!(out, "\nwatching {} — Ctrl-C to stop", root.display());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_files_wake_the_loop() {
        for path in [
            "src/app.ts",
            "src/nested/deep/component.tsx",
            "lanekeep.config.ts",
            "lanekeep/rules/no-debugger.ts",
            "app.py",
        ] {
            assert!(is_interesting(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn the_cache_does_not_wake_the_loop() {
        // The one that matters. lanekeep writes this itself, so reacting to it means
        // re-checking forever at full CPU while looking like it is working.
        assert!(!is_interesting(Path::new(".lanekeep/cache")));
        assert!(!is_interesting(Path::new(
            "/abs/project/.lanekeep/cache.tmp"
        )));
    }

    #[test]
    fn noisy_directories_do_not_wake_the_loop() {
        for path in [
            ".git/index",
            "node_modules/react/index.js",
            "target/debug/lanekeep",
            "dist/bundle.js",
            ".venv/lib/python3.12/site-packages/x.py",
            "src/__pycache__/app.cpython-312.pyc",
        ] {
            assert!(!is_interesting(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn a_directory_is_matched_by_name_at_any_depth() {
        assert!(!is_interesting(Path::new(
            "packages/ui/node_modules/dep/index.js"
        )));
        assert!(!is_interesting(Path::new("apps/mobile/.lanekeep/cache")));
    }

    #[test]
    fn a_file_merely_named_like_an_ignored_directory_still_wakes_it() {
        // `target.ts` is source; `target/` is a build directory. Matching on a path
        // component rather than a substring is what tells them apart.
        assert!(is_interesting(Path::new("src/target.ts")));
        assert!(is_interesting(Path::new("src/node_modules.ts")));
        assert!(is_interesting(Path::new("src/dist.py")));
    }
}
