//! Tracked, confined filesystem reads.
//!
//! The only way a rule reaches a file other than the one it is checking. Two properties have
//! to hold together, and neither is optional:
//!
//! **Confinement.** A read resolves inside the project root or it fails. Traversal is
//! rejected lexically before the filesystem is touched, so `../../../etc/passwd` produces a
//! message about escaping the root rather than a confusing "not found" — and the resolved
//! path is canonicalized and re-checked, so a symlink inside the root pointing outside it is
//! rejected too. A lexical check alone would see an innocent relative path and allow it.
//!
//! **Tracking.** Every read is recorded as `(path, content_hash)`, including reads that
//! found nothing. That record is what a cache entry needs to know when it has gone stale;
//! see [`lanekeep_core::tracked`].
//!
//! # Reads are memoized within a run
//!
//! Reading the same path twice returns the same bytes, even if something rewrote the file in
//! between. A rule that saw a file change under it could report differently on two runs over
//! identical input, which is the determinism invariant — and the cache would record one of
//! the two hashes with no way to say which was used.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use lanekeep_core::tracked::{ContentHash, TrackedRead};
use lanekeep_core::{FilePath, tracked};
use thiserror::Error;

/// Why a read was refused.
///
/// Distinct from "nothing was there", which is an ordinary answer a rule handles.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReadError {
    /// The path resolves outside the project root.
    #[error(
        "cannot read `{path}`\n  \
         it resolves outside the project root, and rules may only read files within it"
    )]
    EscapesRoot {
        /// The path as the rule wrote it.
        path: String,
    },

    /// The path was absolute.
    #[error(
        "cannot read `{path}`\n  \
         reads are relative to the project root — an absolute path would make the rule \
         depend on where the project happens to be checked out"
    )]
    Absolute {
        /// The path as the rule wrote it.
        path: String,
    },

    /// The file exists but is not text.
    #[error(
        "cannot read `{path}` as text: it is not valid UTF-8\n  \
         use ctx.fileExists if the question is whether it is there"
    )]
    NotText {
        /// The path as the rule wrote it.
        path: String,
    },
}

/// What a resolved path turned out to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// The file was read.
    Text(String, ContentHash),
    /// Nothing was there.
    Absent,
    /// It was there and is not text.
    Binary,
}

/// Tracked, confined access to the project's files.
#[derive(Debug)]
pub struct FileAccess {
    root: PathBuf,
    /// Everything resolved so far this file, keyed by project-relative path.
    ///
    /// A `BTreeMap` rather than a hash map: it is small, and iterating it in path order
    /// makes the recorded dependency list deterministic without a separate sort.
    seen: RefCell<BTreeMap<String, Outcome>>,
}

impl FileAccess {
    /// Anchor reads at a project root, canonicalizing it.
    ///
    /// Every containment check compares against the root, so it has to be canonical or a
    /// symlinked checkout would fail every check. Callers that already hold a canonical
    /// root should use [`FileAccess::rooted`] instead — this is one syscall, and the engine
    /// builds an access per file.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self::rooted(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
    }

    /// Anchor reads at an already-canonical root.
    ///
    /// Cheap enough to call per file, which is what the engine does: a fresh access per
    /// file makes it structurally impossible for one file's reads to be recorded against
    /// another's, rather than making it depend on a reset being called in the right place.
    #[must_use]
    pub fn rooted(root: PathBuf) -> Self {
        Self {
            root,
            seen: RefCell::new(BTreeMap::new()),
        }
    }

    /// The project root reads are confined to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read a file's text, or `None` if nothing is there.
    ///
    /// # Errors
    ///
    /// [`ReadError`] if the path escapes the root, is absolute, or holds something that is
    /// not text. Absence is not an error — a rule asking whether a config is present should
    /// not have to catch to find out.
    pub fn read(&self, path: &str) -> Result<Option<String>, ReadError> {
        match self.resolve(path)? {
            Outcome::Text(text, _) => Ok(Some(text)),
            Outcome::Absent => Ok(None),
            Outcome::Binary => Err(ReadError::NotText {
                path: path.to_owned(),
            }),
        }
    }

    /// Whether a file is there.
    ///
    /// A file that exists but is not text still exists — this answers the question asked,
    /// where returning `false` would claim something untrue about the filesystem.
    ///
    /// # Errors
    ///
    /// [`ReadError`] if the path escapes the root or is absolute.
    pub fn exists(&self, path: &str) -> Result<bool, ReadError> {
        Ok(!matches!(self.resolve(path)?, Outcome::Absent))
    }

    /// Everything read so far, in path order.
    #[must_use]
    pub fn dependencies(&self) -> Vec<TrackedRead> {
        let mut reads: Vec<TrackedRead> = self
            .seen
            .borrow()
            .iter()
            .map(|(path, outcome)| {
                let file = FilePath::new(path);
                match outcome {
                    Outcome::Text(_, hash) => TrackedRead::found(file, *hash),
                    // A file that is there but unreadable as text is still a dependency: if
                    // it is replaced with text, the rule's answer changes.
                    Outcome::Binary => TrackedRead::found(file, ContentHash::new([0; 32])),
                    Outcome::Absent => TrackedRead::absent(file),
                }
            })
            .collect();
        tracked::sort(&mut reads);
        reads
    }

    /// Forget everything, for an embedder reusing one access across several files.
    ///
    /// The engine does not use this — it builds an access per file, so there is nothing to
    /// forget. Kept because reuse is a reasonable thing for an embedder to want, and a
    /// half-populated access is not.
    pub fn clear(&self) {
        self.seen.borrow_mut().clear();
    }

    /// Resolve, read and record a path, or return what was already recorded.
    fn resolve(&self, path: &str) -> Result<Outcome, ReadError> {
        let key = normalize_key(path);
        if let Some(outcome) = self.seen.borrow().get(&key) {
            return Ok(outcome.clone());
        }

        let outcome = self.load(path)?;
        self.seen.borrow_mut().insert(key, outcome.clone());
        Ok(outcome)
    }

    /// Do the actual filesystem work, having decided the path is allowed.
    fn load(&self, path: &str) -> Result<Outcome, ReadError> {
        let relative = Path::new(path);
        if relative.is_absolute() || relative.has_root() {
            // `has_root` as well as `is_absolute`, because `\windows\path` is rooted but not
            // absolute on Windows — and a check that passes on one platform and not the
            // other is worse than no check.
            return Err(ReadError::Absolute {
                path: path.to_owned(),
            });
        }

        // Lexically first, so an escape is named as one whether or not the target exists.
        let normalized = normalize(relative);
        if normalized
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ReadError::EscapesRoot {
                path: path.to_owned(),
            });
        }

        let full = self.root.join(&normalized);
        let Ok(canonical) = full.canonicalize() else {
            // Nothing there. Not an error, and deliberately not distinguished from a
            // permission failure: either way the rule cannot see it, and a rule that
            // branched on the difference would give different answers on different machines.
            return Ok(Outcome::Absent);
        };

        // And again after canonicalizing, which is what catches a symlink inside the root
        // pointing outside it. The lexical check above cannot see through one.
        if !canonical.starts_with(&self.root) {
            return Err(ReadError::EscapesRoot {
                path: path.to_owned(),
            });
        }

        let Ok(bytes) = std::fs::read(&canonical) else {
            return Ok(Outcome::Absent);
        };
        let hash = ContentHash::new(*blake3::hash(&bytes).as_bytes());

        match String::from_utf8(bytes) {
            Ok(text) => Ok(Outcome::Text(text, hash)),
            Err(_) => Ok(Outcome::Binary),
        }
    }
}

/// The key a path is recorded under, so `./a.json` and `a.json` are one dependency.
fn normalize_key(path: &str) -> String {
    normalize(Path::new(path))
        .to_string_lossy()
        .replace('\\', "/")
}

/// Resolve `.` and `..` lexically, without consulting the filesystem.
///
/// A traversal attempt has to be rejected with a message about escaping the root whether or
/// not the target happens to exist, which `canonicalize` alone cannot do.
///
/// A leading `..` is kept as a marker so the caller's containment check can see it — and,
/// critically, a later `..` must not pop that marker. `../../etc/passwd` popping its own
/// first `..` would collapse to `etc/passwd`, which looks contained, and the read would
/// then resolve to `<root>/etc/passwd`: not an escape, but silently the wrong file. Depth
/// counts only real segments, so a marker can never be consumed.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                } else {
                    out.push("..");
                }
            }
            other => {
                out.push(other.as_os_str());
                depth += 1;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lanekeep-files-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creates dir");
            let fixture = Self { dir };
            for (path, contents) in files {
                let full = fixture.dir.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).expect("creates parent");
                }
                std::fs::write(full, contents).expect("writes");
            }
            fixture
        }

        fn access(&self) -> FileAccess {
            FileAccess::new(&self.dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn reads_a_file_in_the_root() {
        let fixture = Fixture::new("read", &[("a.json", "{}")]);
        let access = fixture.access();
        assert_eq!(
            access.read("a.json").expect("allowed"),
            Some("{}".to_owned())
        );
    }

    #[test]
    fn reads_a_file_in_a_subdirectory() {
        let fixture = Fixture::new("nested", &[("pkg/a.json", "{\"n\":1}")]);
        let access = fixture.access();
        assert_eq!(
            access.read("pkg/a.json").expect("allowed"),
            Some("{\"n\":1}".to_owned())
        );
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        // A rule asking whether a config is present should not have to catch to find out.
        let fixture = Fixture::new("missing", &[]);
        let access = fixture.access();
        assert_eq!(access.read("nope.json").expect("allowed"), None);
        assert!(!access.exists("nope.json").expect("allowed"));
    }

    #[test]
    fn traversal_out_of_the_root_is_refused() {
        let fixture = Fixture::new("traversal", &[("a.json", "{}")]);
        let access = fixture.access();
        for attempt in ["../outside.json", "../../etc/passwd", "pkg/../../outside"] {
            let error = access.read(attempt).expect_err("is refused");
            assert!(
                matches!(error, ReadError::EscapesRoot { .. }),
                "`{attempt}` gave {error:?}"
            );
        }
    }

    #[test]
    fn traversal_that_comes_back_inside_is_allowed() {
        // `pkg/../a.json` never leaves the root. Refusing it would be a check that punishes
        // spelling rather than one that protects anything.
        let fixture = Fixture::new("returns", &[("a.json", "{}"), ("pkg/b.json", "{}")]);
        let access = fixture.access();
        assert_eq!(
            access.read("pkg/../a.json").expect("allowed"),
            Some("{}".to_owned())
        );
    }

    #[test]
    fn an_absolute_path_is_refused() {
        // Built from `temp_dir` rather than written literally: `/etc/passwd` is absolute on
        // Unix and merely rooted on Windows, so a literal takes a different branch on each.
        let fixture = Fixture::new("absolute", &[]);
        let access = fixture.access();
        let outside = std::env::temp_dir().join("lanekeep-absolute-read-probe.json");
        let error = access
            .read(&outside.display().to_string())
            .expect_err("is refused");
        assert!(matches!(error, ReadError::Absolute { .. }), "{error:?}");
    }

    #[test]
    fn a_read_is_recorded_as_a_dependency() {
        let fixture = Fixture::new("recorded", &[("a.json", "{}")]);
        let access = fixture.access();
        access.read("a.json").expect("allowed");

        let deps = access.dependencies();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].path.as_str(), "a.json");
        assert!(deps[0].hash.is_some(), "a file that was read has a hash");
    }

    #[test]
    fn a_miss_is_recorded_as_a_dependency() {
        // The one that makes a cache wrong rather than cold: the answer "not there" has to
        // be invalidated when the file appears.
        let fixture = Fixture::new("miss-recorded", &[]);
        let access = fixture.access();
        access.exists("tsconfig.json").expect("allowed");

        let deps = access.dependencies();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].path.as_str(), "tsconfig.json");
        assert_eq!(deps[0].hash, None);
    }

    #[test]
    fn a_refused_read_is_not_recorded() {
        // It never produced an answer, so there is nothing for a cache to depend on.
        let fixture = Fixture::new("refused", &[]);
        let access = fixture.access();
        let _ = access.read("../outside.json");
        assert!(access.dependencies().is_empty());
    }

    #[test]
    fn the_same_file_is_one_dependency_however_it_is_spelled() {
        let fixture = Fixture::new("spelling", &[("a.json", "{}")]);
        let access = fixture.access();
        access.read("a.json").expect("allowed");
        access.read("./a.json").expect("allowed");
        access.read("pkg/../a.json").expect("allowed");
        assert_eq!(access.dependencies().len(), 1);
    }

    #[test]
    fn a_second_read_returns_what_the_first_one_saw() {
        // A rule that saw a file change under it could report differently on two runs over
        // identical input, and the cache would record one hash with no way to say which
        // answer used it.
        let fixture = Fixture::new("memoized", &[("a.json", "before")]);
        let access = fixture.access();
        assert_eq!(
            access.read("a.json").expect("allowed").as_deref(),
            Some("before")
        );

        std::fs::write(fixture.dir.join("a.json"), "after").expect("rewrites");
        assert_eq!(
            access.read("a.json").expect("allowed").as_deref(),
            Some("before"),
            "the run must see one version of a file"
        );
    }

    #[test]
    fn a_binary_file_is_refused_as_text_but_exists() {
        let fixture = Fixture::new("binary", &[]);
        std::fs::write(fixture.dir.join("blob.bin"), [0xff, 0xfe, 0x00]).expect("writes");
        let access = fixture.access();

        let error = access.read("blob.bin").expect_err("is refused");
        assert!(matches!(error, ReadError::NotText { .. }), "{error:?}");
        assert!(
            access.exists("blob.bin").expect("allowed"),
            "it is there, whatever it holds"
        );
    }

    #[test]
    fn dependencies_come_back_in_path_order() {
        let fixture = Fixture::new("ordered", &[("b.json", "{}"), ("a.json", "{}")]);
        let access = fixture.access();
        access.read("b.json").expect("allowed");
        access.read("a.json").expect("allowed");
        access.exists("c.json").expect("allowed");

        assert_eq!(
            access
                .dependencies()
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.json", "b.json", "c.json"]
        );
    }

    #[test]
    fn clearing_forgets_everything() {
        let fixture = Fixture::new("cleared", &[("a.json", "{}")]);
        let access = fixture.access();
        access.read("a.json").expect("allowed");
        access.clear();
        assert!(access.dependencies().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        // The reason the check canonicalizes rather than comparing strings: nothing about
        // `escape.json` looks like traversal.
        let fixture = Fixture::new("symlink", &[]);
        let outside = std::env::temp_dir().join("lanekeep-symlink-target.json");
        std::fs::write(&outside, "secrets").expect("writes target");

        std::os::unix::fs::symlink(&outside, fixture.dir.join("escape.json"))
            .expect("creates symlink");

        let access = fixture.access();
        let error = access.read("escape.json").expect_err("is refused");
        assert!(matches!(error, ReadError::EscapesRoot { .. }), "{error:?}");

        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn a_second_parent_does_not_consume_the_first() {
        // The bug this guards: `..` popping the `..` marker its predecessor pushed collapses
        // `../../etc/passwd` to `etc/passwd`, which looks contained. The read would then
        // resolve to `<root>/etc/passwd` — not an escape, but silently the wrong file, and
        // no error anywhere to say so.
        assert_eq!(
            normalize(Path::new("../../etc/passwd")),
            Path::new("../../etc/passwd")
        );
        assert_eq!(normalize(Path::new("../../..")), Path::new("../../.."));
    }

    #[test]
    fn a_parent_after_a_marker_pops_the_real_segment() {
        // `../pkg/..` is still one level up, not two. Depth counts real segments only, so
        // the marker survives and the segment above it does not.
        assert_eq!(normalize(Path::new("../pkg/..")), Path::new(".."));
        assert_eq!(normalize(Path::new("../pkg/../a")), Path::new("../a"));
    }

    #[test]
    fn traversal_that_returns_is_collapsed() {
        assert_eq!(normalize(Path::new("pkg/../a.json")), Path::new("a.json"));
        assert_eq!(normalize(Path::new("./a/./b")), Path::new("a/b"));
    }
}
