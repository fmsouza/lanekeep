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
//!
//! # Reading under an ignored directory
//!
//! A type-aware rule's inputs are declaration files, and on a Node project those live under
//! `node_modules` — a directory this filter ignores wholesale, because a package install
//! writes thousands of files there and none of the rest is ever an input. So the filter takes
//! a second argument: the paths the previous iteration recorded as tracked reads. One of those
//! wakes the loop even under an ignored directory.
//!
//! **Each type provider contributes to that set its own way**, which is what actually makes
//! editing a linked package's `.d.ts` show up. `main.rs`'s `--watch` call site fills it from
//! `lanekeep_engine::Engine::dependency_paths`, a union of two halves. The builtin oracle's
//! reads travel through `Query::files`, so they are already ordinary tracked reads and land in
//! `Outcome::dependency_paths` the same way every other cache-key input does. The `tsc`
//! provider's do not: the compiler reads through its own host, never through `Query::files`, so
//! nothing it consults is a tracked read on any file at all —
//! `lanekeep_types::TypeProvider::dependency_paths` is what it answers instead, from the
//! program listing `begin_run` already builds for the run key. Missing either half leaves that
//! provider's declaration files invisible to this loop, indistinguishable from every other file
//! under an ignored directory.
//!
//! `.lanekeep/` is excluded from that grant explicitly, rather than by the accident of never
//! appearing in it. Reads are confined to the project root and `.lanekeep/` is inside the
//! root, so a rule that reads the cache would put it in the allowlist and the loop above would
//! be back with its guard apparently intact.
//!
//! **Why the allowlist holds project-relative paths.** `notify` reports absolute paths, and on
//! macOS a temporary directory arrives through `/private/var` while the project root was given
//! as `/var` — comparing two absolute paths would silently never match, which is the failure
//! that looks exactly like the bug this exists to fix. [`is_interesting`] makes the event path
//! root-relative first — `strip_prefix` against the canonicalized root, and only if that fails
//! against the canonicalized event path — and then compares it to an allowlist entry for
//! *equality*.
//!
//! It used to compare trailing components instead, and that was too generous by an amount that
//! grew with the project. `tsconfig.json` is one component and is in every `tsc` run's listing,
//! so every `node_modules/*/tsconfig.json` in the tree matched it: `npm install` woke the loop
//! once per package, for files no run had read. Anchoring is also what keeps
//! [`IGNORED_DIRECTORIES`] meaningful, since its names are read off the same relative path — a
//! checkout under `/home/me/build/` had every event ignored while the absolute components
//! decided it.
//!
//! The trailing comparison survives as the fallback for a path the root does not spell: a
//! deleted file cannot be canonicalized, and a prefix may differ in a way neither spelling
//! resolves. A false accept there costs one extra re-check; a false reject is a missed wake,
//! which is the failure that matters — so where nothing anchors, the comparison still errs
//! toward accepting.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
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

/// The one ignored directory that no allowlist can readmit.
///
/// Separate from [`IGNORED_DIRECTORIES`] because it is the only entry there whose exclusion is
/// a correctness requirement rather than a noise filter — see [`is_interesting`].
const NEVER_INTERESTING: &str = ".lanekeep";

/// Whether a changed path should wake the loop.
///
/// Deliberately coarse: it excludes what could never be source rather than trying to decide
/// what *is*. Discovery already applies the config's `include` and `exclude`, and duplicating
/// that judgment here would give two answers to one question — the failure mode being a file
/// the run would check that the watcher ignores, which reads as lanekeep missing a violation.
///
/// `allowlist` is what the previous iteration recorded as tracked reads, as project-relative
/// paths. A path in it wakes the loop even under an ignored directory: a type-aware rule's
/// declaration files live under `node_modules` on any Node project, and ignoring the whole
/// directory means editing one of them produces no re-check, and the stale answer that
/// follows reads as a rule that stopped working. The grant is per path rather than per
/// directory, so a package install writing thousands of files beside the one that was read
/// still wakes nothing.
///
/// **`.lanekeep/` is refused before the allowlist is consulted, and that is load-bearing.**
/// `lanekeep_core::FileAccess` confines a rule's reads to the project root, and `.lanekeep/`
/// is inside the root — so a rule that reads the cache, deliberately or through a glob, puts
/// it in the run's tracked reads and therefore in this allowlist. Checking the allowlist first
/// would hand back the loop that eats itself with the guard apparently still in place.
///
/// **Both judgments are made on the path *relative to the project root*, and that is the whole
/// of what makes either of them mean anything.** [`IGNORED_DIRECTORIES`] holds names like
/// `build`, `dist` and `target`, so a project that happens to live under
/// `/home/me/build/checkout` had every event ignored when the components were read off the
/// absolute path — a watcher that woke for nothing, looking exactly like one that worked. And
/// the allowlist compared by *trailing* components admitted any file whose tail matched an
/// entry: `tsconfig.json` is a single component and is in every `tsc` run's listing, so
/// `npm install` woke the loop once per `node_modules/*/tsconfig.json` in the tree.
///
/// `root` is expected canonical — [`watch_with`] canonicalizes it once — and the event path is
/// canonicalized only if it does not strip as given, which is a syscall the common case never
/// pays. Where neither strips, the path is not under the root as either spells it (a deleted
/// file cannot be canonicalized, and `notify` may report a symlinked prefix the root does not
/// have): there the old trailing-component comparison is used, because a false accept costs one
/// extra re-check and a false reject is a missed wake, which is the failure that matters.
#[must_use]
pub(crate) fn is_interesting(path: &Path, root: &Path, allowlist: &BTreeSet<PathBuf>) -> bool {
    let relative = under(path, root);
    let considered = relative.as_deref().unwrap_or(path);

    let mut ignored = false;
    for component in considered.components() {
        if let Component::Normal(name) = component
            && let Some(name) = name.to_str()
        {
            if name == NEVER_INTERESTING {
                return false;
            }
            if IGNORED_DIRECTORIES.contains(&name) {
                ignored = true;
            }
        }
    }

    if !ignored {
        return true;
    }
    match relative {
        // Anchored: the allowlist holds project-relative paths, so the comparison is equality
        // and a same-named file elsewhere under the root is not admitted.
        Some(relative) => allowlist.contains(&relative),
        None => allowlist.iter().any(|allowed| path.ends_with(allowed)),
    }
}

/// `path` as the project root spells it, or `None` when it is not under the root.
fn under(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix(root) {
        return Some(rest.to_path_buf());
    }
    // The second spelling, and only when the first failed: on macOS a temporary directory is
    // reported through `/private/var` while the root may have been given as `/var`, and the two
    // are the same directory. `canonicalize` fails for a path that has just been deleted, which
    // is a real event and lands in the fallback above.
    let canonical = path.canonicalize().ok()?;
    canonical.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// Run `check` once, then again whenever the project changes, until interrupted.
///
/// `allowlist` is shared with `once`, which replaces its contents with the paths the run
/// recorded as tracked reads. Replaced rather than extended: a path the last run did not read
/// is no longer an input, and accumulating would grow the set for the life of the session.
///
/// # Errors
///
/// Returns an error if the watcher cannot be created or the root cannot be watched. A failing
/// *check* is not an error here: in a loop, a rule that throws is something to report and go
/// back to waiting for, not a reason to tear the session down.
#[expect(
    clippy::needless_pass_by_value,
    reason = "taken by value on purpose: this loop is the Arc's only other owner, so an \
              owned handle here is what lets the caller's clone and this one be the same \
              allocation for the life of the loop, rather than an incidental borrow"
)]
pub(crate) fn watch_with(
    root: &Path,
    allowlist: Arc<Mutex<BTreeSet<PathBuf>>>,
    mut once: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<std::process::ExitCode> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(DEBOUNCE, sender)
        .map_err(|e| anyhow::anyhow!("cannot watch `{}`: {e}", root.display()))?;
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| anyhow::anyhow!("cannot watch `{}`: {e}", root.display()))?;

    // Canonicalized once, here, because every event is compared against it: `notify` reports
    // absolute paths and the root may have been given in a spelling the events do not use.
    let anchor = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    report(&mut once);
    let mut stderr = std::io::stderr();
    announce(&mut stderr, root);

    // The receiver ends when the debouncer is dropped, which happens on Ctrl-C taking the
    // process down. There is no other exit: a foreground loop the user started is a loop the
    // user ends.
    while let Ok(event) = receiver.recv() {
        // Read once per burst rather than per event, and released before the check runs —
        // `once` writes the next iteration's set through the same handle.
        let accepted: BTreeSet<PathBuf> = allowlist
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();

        let changed: HashSet<PathBuf> = match event {
            Ok(events) => events
                .into_iter()
                .map(|event| event.path)
                .filter(|path| is_interesting(path, &anchor, &accepted))
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

    fn nothing() -> BTreeSet<PathBuf> {
        BTreeSet::new()
    }

    fn allowing(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn source_files_wake_the_loop() {
        for path in [
            "src/app.ts",
            "src/nested/deep/component.tsx",
            "lanekeep.config.ts",
            "lanekeep/rules/no-debugger.ts",
            "app.py",
        ] {
            assert!(
                is_interesting(Path::new(path), Path::new(""), &nothing()),
                "{path}"
            );
        }
    }

    #[test]
    fn the_cache_does_not_wake_the_loop() {
        // The one that matters. lanekeep writes this itself, so reacting to it means
        // re-checking forever at full CPU while looking like it is working.
        assert!(!is_interesting(
            Path::new(".lanekeep/cache"),
            Path::new(""),
            &nothing()
        ));
        assert!(!is_interesting(
            Path::new("/abs/project/.lanekeep/cache.tmp"),
            Path::new("/abs/project"),
            &nothing()
        ));
    }

    #[test]
    fn the_cache_does_not_wake_the_loop_even_when_a_rule_read_it() {
        // `FileAccess` confines reads to the project root and `.lanekeep/` is inside it, so a
        // rule calling `ctx.readFile('.lanekeep/cache')` puts that path in the run's tracked
        // reads and therefore in this allowlist. Consulting the allowlist before excluding
        // `.lanekeep` would hand the feedback loop straight back with the guard still
        // apparently in place.
        let allowlist = allowing(&[".lanekeep/cache", ".lanekeep/components/x.wasm"]);
        assert!(!is_interesting(
            Path::new(".lanekeep/cache"),
            Path::new(""),
            &allowlist
        ));
        assert!(!is_interesting(
            Path::new("/abs/project/.lanekeep/cache"),
            Path::new("/abs/project"),
            &allowlist
        ));
        assert!(!is_interesting(
            Path::new("/abs/project/.lanekeep/components/x.wasm"),
            Path::new("/abs/project"),
            &allowlist
        ));
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
            assert!(
                !is_interesting(Path::new(path), Path::new(""), &nothing()),
                "{path}"
            );
        }
    }

    #[test]
    fn a_declaration_the_oracle_read_wakes_the_loop_under_an_ignored_directory() {
        // The whole reason the allowlist exists. A type-aware rule's inputs live under
        // `node_modules` on any Node project, and before this the loop ignored every one of
        // them — editing a linked package's `.d.ts` produced no re-check at all, and the
        // stale answer that followed looked like a rule that had stopped working.
        let allowlist = allowing(&["node_modules/@acme/rates/index.d.ts"]);
        assert!(is_interesting(
            Path::new("/abs/project/node_modules/@acme/rates/index.d.ts"),
            Path::new("/abs/project"),
            &allowlist
        ));
        // The tail is compared by whole components, so a temporary directory reached through
        // a symlinked prefix still matches — which is the case that would otherwise fail on
        // macOS and pass everywhere else.
        assert!(is_interesting(
            Path::new("/private/var/folders/t/x/node_modules/@acme/rates/index.d.ts"),
            Path::new("/var/folders/t/x"),
            &allowlist
        ));
    }

    #[test]
    fn a_neighbor_of_an_allowlisted_file_still_does_not_wake_the_loop() {
        // The grant is per path, not per directory. Installing a package rewrites thousands
        // of files beside the one the oracle read, and admitting the directory would put the
        // event storm back.
        let allowlist = allowing(&["node_modules/@acme/rates/index.d.ts"]);
        assert!(!is_interesting(
            Path::new("/abs/project/node_modules/@acme/rates/README.md"),
            Path::new("/abs/project"),
            &allowlist
        ));
        assert!(!is_interesting(
            Path::new("/abs/project/node_modules/react/index.js"),
            Path::new("/abs/project"),
            &allowlist
        ));
    }

    #[test]
    fn a_directory_is_matched_by_name_at_any_depth() {
        assert!(!is_interesting(
            Path::new("packages/ui/node_modules/dep/index.js"),
            Path::new(""),
            &nothing()
        ));
        assert!(!is_interesting(
            Path::new("apps/mobile/.lanekeep/cache"),
            Path::new(""),
            &nothing()
        ));
    }

    #[test]
    fn a_file_merely_named_like_an_ignored_directory_still_wakes_it() {
        // `target.ts` is source; `target/` is a build directory. Matching on a path
        // component rather than a substring is what tells them apart.
        assert!(is_interesting(
            Path::new("src/target.ts"),
            Path::new(""),
            &nothing()
        ));
        assert!(is_interesting(
            Path::new("src/node_modules.ts"),
            Path::new(""),
            &nothing()
        ));
        assert!(is_interesting(
            Path::new("src/dist.py"),
            Path::new(""),
            &nothing()
        ));
    }

    #[test]
    fn a_single_component_allowlist_entry_admits_only_the_roots_own_file() {
        // `tsconfig.json` is one component and is in every `tsc` run's listing, so a
        // trailing-component comparison admitted `node_modules/<anything>/tsconfig.json` —
        // which means `npm install` woke the loop once per package in the tree, for files no
        // run ever read. The comparison is anchored at the root instead.
        let root = Path::new("/abs/project");
        let allowlist = allowing(&["tsconfig.json"]);
        assert!(
            !is_interesting(
                Path::new("/abs/project/node_modules/some-lib/tsconfig.json"),
                root,
                &allowlist
            ),
            "a package's own tsconfig.json is not the one the run read"
        );
        assert!(
            is_interesting(Path::new("/abs/project/tsconfig.json"), root, &allowlist),
            "and the root's own is"
        );
    }

    #[test]
    fn a_project_under_a_directory_named_like_an_ignored_one_still_wakes() {
        // Read off the absolute path, `IGNORED_DIRECTORIES` ignored *every* event for a
        // checkout under `.../build/`, `.../dist/` or `.../target/` — a watcher that never
        // woke, looking exactly like one that worked. The components that decide this are the
        // root-relative ones.
        for parent in ["build", "dist", "target", "node_modules"] {
            let root = PathBuf::from(format!("/home/me/{parent}/checkout"));
            let path = root.join("src/app.ts");
            assert!(
                is_interesting(&path, &root, &nothing()),
                "a checkout under `{parent}` never wakes for its own source"
            );
        }
    }

    #[test]
    fn a_path_the_root_does_not_spell_falls_back_to_the_trailing_comparison() {
        // The documented fallback: a deleted file cannot be canonicalized, and `notify` may
        // report a prefix the root does not have. A false accept there costs one extra
        // re-check; a false reject is a missed wake.
        let allowlist = allowing(&["node_modules/@acme/rates/index.d.ts"]);
        assert!(is_interesting(
            Path::new("/elsewhere/node_modules/@acme/rates/index.d.ts"),
            Path::new("/abs/project"),
            &allowlist
        ));
    }
}
