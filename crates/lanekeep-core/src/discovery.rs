//! Finding the files to check.
//!
//! Discovery is gitignore-aware, so build output and vendored dependencies are skipped
//! without every project having to exclude them by hand.
//!
//! The returned order is sorted. Nothing downstream depends on it — violations are sorted
//! before reporting — but discovery feeding files to workers in filesystem order would make
//! the *work distribution* vary between runs on identical input, which turns a timing
//! difference into something that looks like nondeterminism when a run breaches a budget.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

use crate::location::FilePath;

/// Why discovery could not run.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiscoveryError {
    /// A glob in `include` or `exclude` is malformed.
    #[error("invalid {field} pattern `{pattern}`: {detail}")]
    InvalidGlob {
        /// Which config field it came from.
        field: &'static str,
        /// The pattern as written.
        pattern: String,
        /// What is wrong with it.
        detail: String,
    },

    /// The project root cannot be walked.
    #[error("cannot read project root `{path}`: {detail}")]
    Unreadable {
        /// The root as given.
        path: String,
        /// What went wrong.
        detail: String,
    },
}

/// Which files a run considers.
#[derive(Debug)]
pub struct Discovery {
    root: PathBuf,
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
}

impl Discovery {
    /// Build a discovery over a project root.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::InvalidGlob`] for a malformed pattern, with the field it
    /// came from — an error naming only the pattern leaves the reader searching for it.
    pub fn new(
        root: impl AsRef<Path>,
        include: &[String],
        exclude: &[String],
    ) -> Result<Self, DiscoveryError> {
        let root = root.as_ref();
        let canonical = root
            .canonicalize()
            .map_err(|e| DiscoveryError::Unreadable {
                path: root.display().to_string(),
                detail: e.to_string(),
            })?;

        Ok(Self {
            root: canonical,
            include: build_set(include, "include")?,
            exclude: build_set(exclude, "exclude")?,
            has_include: !include.is_empty(),
        })
    }

    /// The project root, canonicalized.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a path relative to the root is selected.
    ///
    /// Exclusion wins over inclusion: a project listing a broad `include` and a narrow
    /// `exclude` means the exclusion, and the other order would make `exclude` useless.
    #[must_use]
    pub fn selects(&self, relative: &FilePath) -> bool {
        let path = relative.as_str();
        if self.exclude.is_match(path) {
            return false;
        }
        // No `include` at all means everything the walk turned up, which is the useful
        // default for `lanekeep check` in a small project.
        !self.has_include || self.include.is_match(path)
    }

    /// Every selected file, sorted.
    ///
    /// Infallible: the root was canonicalized when this was built, and a single unreadable
    /// entry is skipped rather than failing a run over a tree that may contain anything.
    #[must_use]
    pub fn walk(&self) -> Vec<FilePath> {
        let mut out = Vec::new();

        for entry in ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            // Honor .gitignore even outside a repository. The walker otherwise treats
            // ignore files as meaningless without a .git directory, which would make
            // discovery depend on whether the project happens to be checked out — the
            // same tree giving different answers in a tarball than in a clone.
            .require_git(false)
            .build()
        {
            // A single unreadable entry is not a reason to fail the run.
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&self.root) else {
                continue;
            };

            let relative = FilePath::new(relative);
            if self.selects(&relative) {
                out.push(relative);
            }
        }

        out.sort();
        out.dedup();
        out
    }
}

fn build_set(patterns: &[String], field: &'static str) -> Result<GlobSet, DiscoveryError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| DiscoveryError::InvalidGlob {
            field,
            pattern: pattern.clone(),
            detail: e.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| DiscoveryError::InvalidGlob {
        field,
        pattern: patterns.join(", "),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, files: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!("lanekeep-discovery-{name}"));
            let _ = fs::remove_dir_all(&dir);
            for path in files {
                let full = dir.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).expect("creates parent");
                }
                fs::write(&full, "const x = 1;\n").expect("writes");
            }
            fs::create_dir_all(&dir).expect("creates dir");
            Self { dir }
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("creates parent");
            }
            fs::write(full, contents).expect("writes");
        }

        fn walk(&self, include: &[&str], exclude: &[&str]) -> Vec<String> {
            let include: Vec<String> = include.iter().map(|s| (*s).to_owned()).collect();
            let exclude: Vec<String> = exclude.iter().map(|s| (*s).to_owned()).collect();
            Discovery::new(&self.dir, &include, &exclude)
                .expect("builds")
                .walk()
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn finds_files_matching_include() {
        let fixture = Fixture::new(
            "include",
            &["src/a.ts", "src/b.tsx", "src/c.md", "other/d.ts"],
        );
        assert_eq!(fixture.walk(&["src/**/*.ts"], &[]), ["src/a.ts"]);
    }

    #[test]
    fn no_include_selects_everything_found() {
        let fixture = Fixture::new("no-include", &["a.ts", "b.md"]);
        let found = fixture.walk(&[], &[]);
        assert!(found.contains(&"a.ts".to_owned()));
        assert!(found.contains(&"b.md".to_owned()));
    }

    #[test]
    fn exclude_wins_over_include() {
        // The other order would make `exclude` useless, since anything excluded is by
        // definition something `include` matched.
        let fixture = Fixture::new("exclude", &["src/a.ts", "src/a.test.ts"]);
        assert_eq!(
            fixture.walk(&["src/**/*.ts"], &["**/*.test.ts"]),
            ["src/a.ts"]
        );
    }

    #[test]
    fn respects_gitignore() {
        let fixture = Fixture::new("gitignore", &["src/a.ts", "dist/b.ts"]);
        fixture.write(".gitignore", "dist/\n");

        let found = fixture.walk(&["**/*.ts"], &[]);
        assert!(found.contains(&"src/a.ts".to_owned()));
        assert!(
            !found.contains(&"dist/b.ts".to_owned()),
            "gitignored files must not be checked: {found:?}"
        );
    }

    #[test]
    fn the_order_is_sorted_and_stable() {
        // Nothing downstream depends on this order, but feeding workers in filesystem
        // order would make work distribution vary run to run — which looks like
        // nondeterminism the moment a run breaches a budget.
        let fixture = Fixture::new("order", &["z.ts", "a.ts", "m/n.ts", "b.ts"]);
        let first = fixture.walk(&["**/*.ts"], &[]);
        assert_eq!(first, ["a.ts", "b.ts", "m/n.ts", "z.ts"]);

        for _ in 0..5 {
            assert_eq!(fixture.walk(&["**/*.ts"], &[]), first);
        }
    }

    #[test]
    fn reports_a_bad_glob_with_the_field_it_came_from() {
        let fixture = Fixture::new("bad-glob", &["a.ts"]);
        let err =
            Discovery::new(&fixture.dir, &["src/[".to_owned()], &[]).expect_err("malformed glob");

        match err {
            DiscoveryError::InvalidGlob { field, pattern, .. } => {
                assert_eq!(field, "include");
                assert_eq!(pattern, "src/[");
            }
            DiscoveryError::Unreadable { .. } => panic!("wrong error variant"),
        }

        let err =
            Discovery::new(&fixture.dir, &[], &["**/[".to_owned()]).expect_err("malformed glob");
        assert!(
            matches!(
                err,
                DiscoveryError::InvalidGlob {
                    field: "exclude",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_missing_root_is_reported() {
        let err = Discovery::new("/definitely/not/here", &[], &[]).expect_err("no such root");
        assert!(matches!(err, DiscoveryError::Unreadable { .. }), "{err:?}");
    }

    #[test]
    fn selects_can_be_asked_without_walking() {
        let fixture = Fixture::new("selects", &["a.ts"]);
        let discovery = Discovery::new(
            &fixture.dir,
            &["src/**/*.ts".to_owned()],
            &["**/*.test.ts".to_owned()],
        )
        .expect("builds");

        assert!(discovery.selects(&FilePath::new("src/a.ts")));
        assert!(!discovery.selects(&FilePath::new("src/a.test.ts")));
        assert!(!discovery.selects(&FilePath::new("other/a.ts")));
    }
}
