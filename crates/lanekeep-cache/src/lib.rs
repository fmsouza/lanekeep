//! Content-addressed result cache with dependency tracking for lanekeep.
//!
//! A single-file, content-addressed store holding the violations, facts and tracked read
//! dependencies of each file.
//!
//! The cache is disposable by design: any read error means a cold recompute, never a
//! failure. That is what makes a purpose-built on-disk format acceptable rather than
//! reckless — nothing here can break a run, so the worst a format bug can do is cost time.
//!
//! # A hit needs two things
//!
//! 1. **The key matches** — same engine, same host API, same grammar, same ruleset, same
//!    config, same path, same bytes. See [`key`].
//! 2. **Every dependency still hashes the same** — because `ctx.readFile` lets a result
//!    depend on files other than the one being checked. See [`validate`].
//!
//! The second is what makes the first safe. Without it a rule that read `package.json` would
//! keep its verdict after `package.json` changed, and nothing about the checked file would
//! have changed to say otherwise.
//!
//! # The asymmetry that shapes everything here
//!
//! Over-invalidating costs a recompute. Under-invalidating reports a stale answer and gives
//! no sign it did — the output looks exactly like a correct one. So every doubtful input
//! goes in the key, one damaged entry discards the whole file, and a dependency that cannot
//! be hashed counts as changed.

pub mod entry;
pub mod key;
pub mod store;

use std::path::Path;

use lanekeep_core::ContentHash;

pub use entry::Entry;
pub use key::{CacheKey, FORMAT_VERSION, GrammarKey, RunKey};
pub use store::Store;

/// Hash a file's bytes for use as a cache-key input.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash::new(*blake3::hash(bytes).as_bytes())
}

/// Whether every dependency an entry recorded still holds.
///
/// `root` is the project root the recorded paths are relative to.
///
/// A dependency that cannot be read now counts as changed, whether it was recorded as
/// present or absent. Permissions, a vanished directory, a race — none of them are grounds
/// for trusting a cached answer, and the cost of being wrong is a recompute.
#[must_use]
pub fn validate(entry: &Entry, root: &Path) -> bool {
    entry.dependencies.iter().all(|read| {
        let current = std::fs::read(root.join(read.path.as_str()))
            .ok()
            .map(|bytes| hash_bytes(&bytes));

        match (read.hash, current) {
            // It was there and still hashes the same.
            (Some(recorded), Some(now)) => recorded == now,
            // It was not there and still is not. This is the case a cache is wrong without:
            // a rule that branched on absence has to be reconsidered when the file appears.
            (None, None) => true,
            // Appeared, or vanished, or became unreadable.
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lanekeep_core::FilePath;
    use lanekeep_core::tracked::TrackedRead;

    use super::*;

    struct Project {
        dir: PathBuf,
    }

    impl Project {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("lanekeep-validate-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creates dir");
            let project = Self { dir };
            for (path, contents) in files {
                project.write(path, contents);
            }
            project
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("creates parent");
            }
            std::fs::write(full, contents).expect("writes");
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn entry_depending_on(reads: Vec<TrackedRead>) -> Entry {
        Entry {
            dependencies: reads,
            ..Entry::default()
        }
    }

    #[test]
    fn an_entry_with_no_dependencies_is_always_valid() {
        let project = Project::new("none", &[]);
        assert!(validate(&Entry::default(), &project.dir));
    }

    #[test]
    fn an_unchanged_dependency_holds() {
        let project = Project::new("unchanged", &[("package.json", "{}")]);
        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("package.json"),
            hash_bytes(b"{}"),
        )]);
        assert!(validate(&entry, &project.dir));
    }

    #[test]
    fn a_changed_dependency_invalidates() {
        let project = Project::new("changed", &[("package.json", "{\"type\":\"module\"}")]);
        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("package.json"),
            hash_bytes(b"{}"),
        )]);
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn a_vanished_dependency_invalidates() {
        let project = Project::new("vanished", &[]);
        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("package.json"),
            hash_bytes(b"{}"),
        )]);
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn an_absent_dependency_that_is_still_absent_holds() {
        let project = Project::new("still-absent", &[]);
        let entry = entry_depending_on(vec![TrackedRead::absent(FilePath::new("tsconfig.json"))]);
        assert!(validate(&entry, &project.dir));
    }

    #[test]
    fn a_dependency_that_appeared_invalidates() {
        // The case that makes a cache wrong rather than merely cold. A rule told
        // `tsconfig.json` was absent must be reconsidered once it exists — and nothing
        // about the checked file changed to say so.
        let project = Project::new("appeared", &[("tsconfig.json", "{}")]);
        let entry = entry_depending_on(vec![TrackedRead::absent(FilePath::new("tsconfig.json"))]);
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn one_changed_dependency_among_many_invalidates() {
        let project = Project::new(
            "one-of-many",
            &[("a.json", "{}"), ("b.json", "changed"), ("c.json", "{}")],
        );
        let entry = entry_depending_on(vec![
            TrackedRead::found(FilePath::new("a.json"), hash_bytes(b"{}")),
            TrackedRead::found(FilePath::new("b.json"), hash_bytes(b"{}")),
            TrackedRead::found(FilePath::new("c.json"), hash_bytes(b"{}")),
        ]);
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn a_directory_where_a_file_was_invalidates() {
        // Reading a directory fails, which counts as changed rather than as unchanged.
        let project = Project::new("directory", &[]);
        std::fs::create_dir_all(project.dir.join("package.json")).expect("creates dir");
        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("package.json"),
            hash_bytes(b"{}"),
        )]);
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn identical_bytes_hash_identically() {
        assert_eq!(hash_bytes(b"hello"), hash_bytes(b"hello"));
        assert_ne!(hash_bytes(b"hello"), hash_bytes(b"hellp"));
    }
}
