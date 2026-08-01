//! Selecting files from git, for `--since` and `--staged`.
//!
//! These are the incremental entry points from `docs/architecture.md` §8.4. They answer
//! "what did I touch", which is a different question from "what is stale" — the cache
//! answers the second, by content hash, and would still do the right thing without these.
//! What they save is reading and hashing every file in the corpus to find that out.
//!
//! # Shelling out to git
//!
//! Rather than linking a git implementation. The user's `git` already agrees with their
//! configuration — worktrees, submodules, `core.excludesFile`, a `.gitattributes` that
//! affects nothing here but might later — and a second implementation would agree until it
//! did not. It is also one dependency instead of a large one.
//!
//! A failure is reported rather than swallowed. If someone asks for `--since main` and
//! there is no `main`, checking everything instead would be a surprising amount of work
//! done silently, and checking nothing would look like a clean run.

use std::path::Path;
use std::process::Command;

use thiserror::Error;

use crate::location::FilePath;

/// Why a selection could not be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChangeError {
    /// `git` could not be run at all.
    #[error(
        "cannot run git: {detail}\n  \
         --since and --staged ask git which files changed, so they need it on PATH"
    )]
    Unavailable {
        /// What the operating system said.
        detail: String,
    },

    /// `git` ran and refused.
    #[error("git could not list changed files: {detail}")]
    Refused {
        /// What git wrote to stderr, trimmed.
        detail: String,
    },
}

/// Files changed against a git ref, including untracked ones.
///
/// Untracked files are included because a file you just created is a file you just changed,
/// and a pre-commit check that ignored new files would miss the most likely place for a new
/// violation.
///
/// # Errors
///
/// [`ChangeError`] if git is missing or the ref does not resolve.
pub fn since(root: &Path, reference: &str) -> Result<Vec<FilePath>, ChangeError> {
    let mut paths = run_git(root, &["diff", "--name-only", "--relative", reference])?;
    paths.extend(untracked(root)?);
    Ok(normalize(root, paths))
}

/// Files staged in the index.
///
/// The pre-commit default: exactly what is about to be committed, which is not the same as
/// what is in the working tree.
///
/// # Errors
///
/// As [`since`].
pub fn staged(root: &Path) -> Result<Vec<FilePath>, ChangeError> {
    let paths = run_git(root, &["diff", "--cached", "--name-only", "--relative"])?;
    Ok(normalize(root, paths))
}

/// Files git knows nothing about yet, respecting ignore rules.
fn untracked(root: &Path) -> Result<Vec<String>, ChangeError> {
    run_git(
        root,
        &["ls-files", "--others", "--exclude-standard", "--", "."],
    )
}

/// Drop what no longer exists, canonicalize separators, sort, and dedupe.
///
/// A rename or a delete shows up in `git diff` as a path that is not there any more.
/// Checking it would be an error about a missing file for something the user did on
/// purpose.
///
/// Sorting matters for the same reason it matters everywhere else here: two runs over the
/// same working tree must produce the same list, and git's output order is not something to
/// depend on.
fn normalize(root: &Path, paths: Vec<String>) -> Vec<FilePath> {
    let mut selected: Vec<FilePath> = paths
        .into_iter()
        .filter(|path| !path.is_empty())
        .filter(|path| root.join(path).is_file())
        .map(|path| FilePath::new(&path))
        .collect();
    selected.sort();
    selected.dedup();
    selected
}

/// Run git in the project root and split its output into lines.
fn run_git(root: &Path, args: &[&str]) -> Result<Vec<String>, ChangeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| ChangeError::Unavailable {
            detail: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(ChangeError::Refused {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Repo {
        dir: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("lanekeep-changed-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creates dir");
            let repo = Self { dir };

            repo.git(&["init", "--quiet"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo.git(&["config", "user.name", "Test"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(args)
                .output()
                .expect("runs git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("creates parent");
            }
            std::fs::write(full, contents).expect("writes");
        }

        fn commit(&self, message: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "--quiet", "-m", message]);
        }

        fn names(paths: &[FilePath]) -> Vec<&str> {
            paths.iter().map(FilePath::as_str).collect()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn since_lists_files_changed_against_a_ref() {
        let repo = Repo::new("since");
        repo.write("a.ts", "const a = 1;\n");
        repo.write("b.ts", "const b = 1;\n");
        repo.commit("first");

        repo.write("a.ts", "const a = 2;\n");
        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert_eq!(Repo::names(&changed), vec!["a.ts"]);
    }

    #[test]
    fn since_includes_untracked_files() {
        // A file you just created is a file you just changed. A pre-commit check that
        // ignored new files would miss the likeliest place for a new violation.
        let repo = Repo::new("untracked");
        repo.write("a.ts", "const a = 1;\n");
        repo.commit("first");

        repo.write("new.ts", "const n = 1;\n");
        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert_eq!(Repo::names(&changed), vec!["new.ts"]);
    }

    #[test]
    fn since_respects_gitignore() {
        let repo = Repo::new("ignored");
        repo.write(".gitignore", "ignored/\n");
        repo.write("a.ts", "const a = 1;\n");
        repo.commit("first");

        repo.write("ignored/x.ts", "const x = 1;\n");
        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert!(
            Repo::names(&changed).is_empty(),
            "an ignored file was selected: {:?}",
            Repo::names(&changed)
        );
    }

    #[test]
    fn a_deleted_file_is_not_selected() {
        // It shows in `git diff` as a path that is no longer there. Checking it would be an
        // error about a missing file for something the user did on purpose.
        let repo = Repo::new("deleted");
        repo.write("a.ts", "const a = 1;\n");
        repo.write("b.ts", "const b = 1;\n");
        repo.commit("first");

        std::fs::remove_file(repo.dir.join("b.ts")).expect("removes");
        repo.write("a.ts", "const a = 2;\n");

        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert_eq!(Repo::names(&changed), vec!["a.ts"]);
    }

    #[test]
    fn a_directory_is_not_selected() {
        let repo = Repo::new("directory");
        repo.write("a.ts", "const a = 1;\n");
        repo.commit("first");
        std::fs::create_dir_all(repo.dir.join("subdir")).expect("creates");

        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert!(Repo::names(&changed).is_empty());
    }

    #[test]
    fn staged_lists_only_the_index() {
        // Not the same as the working tree, which is the whole point for a pre-commit hook:
        // what is about to be committed is what should be checked.
        let repo = Repo::new("staged");
        repo.write("a.ts", "const a = 1;\n");
        repo.write("b.ts", "const b = 1;\n");
        repo.commit("first");

        repo.write("a.ts", "const a = 2;\n");
        repo.write("b.ts", "const b = 2;\n");
        repo.git(&["add", "a.ts"]);

        let changed = staged(&repo.dir).expect("lists");
        assert_eq!(Repo::names(&changed), vec!["a.ts"]);
    }

    #[test]
    fn staged_is_empty_with_nothing_staged() {
        let repo = Repo::new("staged-empty");
        repo.write("a.ts", "const a = 1;\n");
        repo.commit("first");
        repo.write("a.ts", "const a = 2;\n");

        assert!(staged(&repo.dir).expect("lists").is_empty());
    }

    #[test]
    fn an_unknown_ref_is_an_error() {
        // Checking everything instead would be a surprising amount of work done silently;
        // checking nothing would look like a clean run.
        let repo = Repo::new("bad-ref");
        repo.write("a.ts", "const a = 1;\n");
        repo.commit("first");

        let error = since(&repo.dir, "no-such-ref").expect_err("refuses");
        assert!(matches!(error, ChangeError::Refused { .. }), "{error:?}");
    }

    #[test]
    fn results_are_sorted_and_deduplicated() {
        // Two runs over the same working tree must produce the same list, and git's output
        // order is not something to depend on.
        let repo = Repo::new("sorted");
        repo.write("z.ts", "const z = 1;\n");
        repo.write("a.ts", "const a = 1;\n");
        repo.write("m.ts", "const m = 1;\n");
        repo.commit("first");

        repo.write("z.ts", "const z = 2;\n");
        repo.write("a.ts", "const a = 2;\n");
        repo.write("m.ts", "const m = 2;\n");

        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert_eq!(Repo::names(&changed), vec!["a.ts", "m.ts", "z.ts"]);
    }

    #[test]
    fn a_nested_path_keeps_its_directory() {
        let repo = Repo::new("nested");
        repo.write("src/deep/a.ts", "const a = 1;\n");
        repo.commit("first");
        repo.write("src/deep/a.ts", "const a = 2;\n");

        let changed = since(&repo.dir, "HEAD").expect("lists");
        assert_eq!(Repo::names(&changed), vec!["src/deep/a.ts"]);
    }

    #[test]
    fn outside_a_repository_is_an_error() {
        let dir = std::env::temp_dir().join(format!("lanekeep-not-a-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");

        let error = staged(&dir).expect_err("refuses");
        assert!(matches!(error, ChangeError::Refused { .. }), "{error:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
