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

use lanekeep_core::files::{Listing, list_directory, listing_hash, normalize};
use lanekeep_core::{ContentHash, ReadOutcome};

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
///
/// # It reproduces the decision `FileAccess` made, rather than making a simpler one
///
/// Reads here go through the same confinement a rule's read went through: resolve, and only
/// then look at what is there. A bare `std::fs::read(root.join(path))` is the obvious
/// spelling and is wrong twice over. It **follows a symlink out of the project**, so a
/// validator reads bytes the rule was refused — and it cannot tell a path that is still
/// refused from one that has appeared, so every importer of a store-linked package (pnpm's
/// `node_modules/pkg`, a workspace link) missed the cache on every run, forever, with nothing
/// in the output to say why.
///
/// So each recorded outcome is checked against the outcome the same path would produce now,
/// and they have to be the same one: refused *and still escaping* holds; refused and now a
/// real in-root directory does not, because `npm install` replacing the link is exactly the
/// change the entry depended on.
#[must_use]
pub fn validate(entry: &Entry, root: &Path) -> bool {
    // Canonical, because every containment check compares against it: a symlinked checkout
    // would otherwise fail every one of them and invalidate the whole cache. The same
    // fallback `FileAccess::new` uses, for the same reason — a root that cannot be
    // canonicalized is a root nothing resolves under, and the reads below then answer absent.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // One equality rather than a table, because the three cases really are one question. It
    // was there and still hashes the same; it was not there and still is not, which is the
    // case a cache is wrong without; or it left the root through a symlink and still does, so
    // a rule would be refused again. Everything else — appeared, vanished, became unreadable,
    // stopped escaping, started escaping — is a change, and every one of those is a `!=`.
    //
    // A listing is the same equality over a different question: the recorded outcome says
    // which one the rule asked, and `current_listing` asks it again.
    entry.dependencies.iter().all(|read| {
        let now = match read.outcome {
            ReadOutcome::Listed(_) | ReadOutcome::Unlisted => {
                current_listing(&root, read.path.as_str())
            }
            ReadOutcome::Found(_) | ReadOutcome::Absent | ReadOutcome::Refused => {
                current(&root, read.path.as_str())
            }
        };
        read.outcome == now
    })
}

/// What listing `path` under `root` would answer now.
///
/// Through `lanekeep_core::files::list_directory`, the function the rule's own listing went
/// through, rather than a second reading of what a listing is — so an unchanged directory cannot
/// invalidate on a difference between two implementations. A recorded refusal is not routed
/// here: refusal is decided by `canonicalize` alone, identically for a file and a directory, so
/// [`current`] already answers it.
fn current_listing(root: &Path, path: &str) -> ReadOutcome {
    match list_directory(root, &normalize(Path::new(path))) {
        Listing::Listed(entries) => ReadOutcome::Listed(listing_hash(&entries)),
        Listing::Unlisted => ReadOutcome::Unlisted,
        Listing::Refused => ReadOutcome::Refused,
    }
}

/// What reading `path` under `root` would answer now, confined as a rule's read is.
///
/// Deliberately the same three answers a tracked read carries, so validation is an equality
/// rather than a table of special cases. This is the one place `lanekeep-cache` touches the
/// filesystem directly: `canonicalize`, a containment check, and a read of what is inside.
fn current(root: &Path, path: &str) -> ReadOutcome {
    let Ok(canonical) = root.join(path).canonicalize() else {
        // Nothing there — or nothing reachable, which a rule's own read cannot tell apart
        // either.
        return ReadOutcome::Absent;
    };
    if !canonical.starts_with(root) {
        // A symlink out of the project. Refused unread, exactly as `FileAccess::load` refuses
        // it, and *before* any bytes are touched: this is what keeps the validator inside the
        // root even when the filesystem does not.
        return ReadOutcome::Refused;
    }
    match std::fs::read(&canonical) {
        Ok(bytes) => ReadOutcome::Found(hash_bytes(&bytes)),
        Err(_) => ReadOutcome::Absent,
    }
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

    /// A binary dependency holds while its bytes are unchanged, and invalidates when they
    /// are not.
    ///
    /// `FileAccess` cannot hand a rule the text of a binary file, but the file is still a
    /// dependency — replace an image with text and a rule's answer can change. It is recorded
    /// with the digest of its bytes, which is exactly what `current` recomputes: for a while a
    /// zero placeholder stood there instead, a value no real file hashes to, so an entry
    /// naming a binary dependency was invalid on every run however still the file lay.
    #[test]
    fn a_binary_dependency_holds_until_its_bytes_change() {
        let project = Project::new("binary", &[]);
        let bytes = [0xff_u8, 0xfe, 0x00, 0x01];
        std::fs::write(project.dir.join("logo.png"), bytes).expect("writes");
        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("logo.png"),
            hash_bytes(&bytes),
        )]);
        assert!(validate(&entry, &project.dir), "the bytes are unchanged");

        std::fs::write(project.dir.join("logo.png"), [0xff_u8, 0xfe, 0x00, 0x02]).expect("writes");
        assert!(!validate(&entry, &project.dir), "and now they are not");
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

    /// A refusal that still escapes the root holds, and the validator never reads the target.
    ///
    /// The case the third outcome exists for. `node_modules/pkg` as pnpm links it resolves out
    /// of the project, so every read under it was refused unread; recorded as an absence, the
    /// validator looked for the file, followed the link, found it, and invalidated — on every
    /// run, for every importer of a linked package, with nothing in the output to say why.
    #[cfg(unix)]
    #[test]
    fn a_refused_dependency_that_still_escapes_holds() {
        let project = Project::new("refused-still-escaping", &[]);
        let outside = std::env::temp_dir().join(format!(
            "lanekeep-validate-refused-target-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&outside).expect("creates the target directory");
        std::fs::write(
            outside.join("index.d.ts"),
            "export declare const x: number;\n",
        )
        .expect("writes the target");
        std::fs::create_dir_all(project.dir.join("node_modules")).expect("creates node_modules");
        std::os::unix::fs::symlink(&outside, project.dir.join("node_modules/pkg"))
            .expect("creates the link");

        let entry = entry_depending_on(vec![TrackedRead::refused(FilePath::new(
            "node_modules/pkg/index.d.ts",
        ))]);
        assert!(
            validate(&entry, &project.dir),
            "the read would be refused again, so the answer that rested on it stands"
        );

        let _ = std::fs::remove_dir_all(&outside);
    }

    /// And `npm install` replacing the link with a real directory invalidates.
    ///
    /// The half that says the row above measures the refusal rather than a validator that
    /// holds everything: the path is now inside the root and readable, so the `undefined` the
    /// refusal produced is no longer the answer.
    #[cfg(unix)]
    #[test]
    fn a_refused_dependency_that_became_a_real_file_invalidates() {
        let project = Project::new("refused-now-real", &[]);
        project.write(
            "node_modules/pkg/index.d.ts",
            "export declare const x: number;\n",
        );

        let entry = entry_depending_on(vec![TrackedRead::refused(FilePath::new(
            "node_modules/pkg/index.d.ts",
        ))]);
        assert!(!validate(&entry, &project.dir));
    }

    /// And a refusal whose link is gone entirely invalidates too.
    ///
    /// Absent is not refused: a rule reading this path now would be told "nothing there"
    /// rather than be refused, and that is a different answer from the one recorded.
    #[test]
    fn a_refused_dependency_that_vanished_invalidates() {
        let project = Project::new("refused-vanished", &[]);
        let entry = entry_depending_on(vec![TrackedRead::refused(FilePath::new(
            "node_modules/pkg/index.d.ts",
        ))]);
        assert!(!validate(&entry, &project.dir));
    }

    /// A dependency that was read and now leaves the root invalidates.
    ///
    /// The mirror of the refusal rows: a path replaced by a symlink out of the project is a
    /// path a rule can no longer read, so an entry that read it is answering about bytes
    /// nothing would hand it now — and the validator must decide that without reading them.
    #[cfg(unix)]
    #[test]
    fn a_found_dependency_that_now_escapes_the_root_invalidates() {
        let project = Project::new("found-now-escaping", &[]);
        let outside = std::env::temp_dir().join(format!(
            "lanekeep-validate-escaped-target-{}.json",
            std::process::id()
        ));
        std::fs::write(&outside, "{}").expect("writes the target");
        std::os::unix::fs::symlink(&outside, project.dir.join("package.json"))
            .expect("creates the link");

        let entry = entry_depending_on(vec![TrackedRead::found(
            FilePath::new("package.json"),
            hash_bytes(b"{}"),
        )]);
        assert!(
            !validate(&entry, &project.dir),
            "the bytes are identical and the read is not one a rule could make"
        );

        let _ = std::fs::remove_file(&outside);
    }

    // --- listings ----------------------------------------------------------------------------

    /// What a rule's `listDir(path)` would record over `project` now.
    fn listing_of(project: &Project, path: &str) -> TrackedRead {
        let access = lanekeep_core::files::FileAccess::new(&project.dir);
        let _ = access.list(path);
        access
            .dependencies()
            .into_iter()
            .next()
            .expect("a listing is always recorded")
    }

    #[test]
    fn an_unchanged_listing_holds() {
        let project = Project::new("list-unchanged", &[("src/a.ts", ""), ("src/sub/b.ts", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, "src")]);
        assert!(validate(&entry, &project.dir));
    }

    #[test]
    fn an_entry_added_to_a_listed_directory_invalidates() {
        // The issue's own case: a rule that listed `src/dir` and saw no `index.ts` has to be
        // reconsidered once one is added, though nothing about the checked file changed.
        let project = Project::new("list-added", &[("src/dir/note.md", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, "src/dir")]);
        project.write("src/dir/index.ts", "");
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn an_entry_removed_from_a_listed_directory_invalidates() {
        let project = Project::new("list-removed", &[("src/a.ts", ""), ("src/b.ts", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, "src")]);
        std::fs::remove_file(project.dir.join("src/b.ts")).expect("removes");
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn an_entry_retyped_from_file_to_directory_invalidates() {
        let project = Project::new("list-retyped", &[("src/thing", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, "src")]);
        std::fs::remove_file(project.dir.join("src/thing")).expect("removes");
        project.write("src/thing/index.ts", "");
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn a_directory_that_appeared_invalidates_a_listing_that_found_none() {
        // A listing recorded as an absent *file* would hold here: the validator's file read of a
        // directory fails, which reads as absent. That is why a listing is its own outcome.
        let project = Project::new("list-appeared", &[]);
        let entry = entry_depending_on(vec![listing_of(&project, "src/dir")]);
        assert_eq!(entry.dependencies[0].outcome, ReadOutcome::Unlisted);
        assert!(validate(&entry, &project.dir), "still absent");

        project.write("src/dir/index.ts", "");
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn a_directory_replacing_a_listed_file_invalidates() {
        let project = Project::new("list-file-to-dir", &[("thing", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, "thing")]);
        assert_eq!(entry.dependencies[0].outcome, ReadOutcome::Unlisted);
        std::fs::remove_file(project.dir.join("thing")).expect("removes");
        std::fs::create_dir_all(project.dir.join("thing")).expect("creates dir");
        assert!(!validate(&entry, &project.dir));
    }

    #[test]
    fn lanekeep_writing_its_cache_does_not_invalidate_a_root_listing() {
        // A cold run creates `.lanekeep/`; the warm run after it validates the entries that
        // cold run wrote. Were the directory visible, every root listing would miss once and
        // answer differently the second time.
        let project = Project::new("list-cache-dir", &[("a.ts", "")]);
        let entry = entry_depending_on(vec![listing_of(&project, ".")]);
        project.write(".lanekeep/cache", "bytes");
        assert!(validate(&entry, &project.dir));
    }

    #[test]
    fn identical_bytes_hash_identically() {
        assert_eq!(hash_bytes(b"hello"), hash_bytes(b"hello"));
        assert_ne!(hash_bytes(b"hello"), hash_bytes(b"hellp"));
    }
}
