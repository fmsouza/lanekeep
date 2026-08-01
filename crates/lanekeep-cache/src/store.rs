//! The on-disk store: one file, read whole, written by atomic rename.
//!
//! # One file, not one per entry
//!
//! At a couple of thousand files, inode churn dominates: opening, stat-ing and closing a
//! file per cache entry costs more than the work being cached. A single file is one open and
//! one read.
//!
//! The architecture specifies memory-mapping it. This reads it instead, because the mapping
//! APIs require `unsafe` and the workspace denies `unsafe_code` — trading a lint that holds
//! everywhere for a performance claim nothing has measured yet would be the wrong way round.
//! A whole-file read is a single sequential I/O of a few megabytes; if a benchmark ever shows
//! it on the critical path, that is the moment to revisit both decisions together.
//!
//! # Nothing here can fail a run
//!
//! Every operation degrades to "no cache". A missing file, a corrupt one, an unwritable
//! directory, a file written by a different build — all mean recompute. This is why the
//! store's fallible operations return `Option` and its write returns `()`: there is no error
//! a caller could usefully act on, and one that stopped a run would make the cache a
//! liability rather than an optimization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::key::{CacheKey, FORMAT_VERSION};

/// Identifies the file as ours before anything else is believed about it.
const MAGIC: &[u8; 8] = b"LKCACHE\x02";

/// Where the cache lives, relative to the project root.
const CACHE_PATH: &str = ".lanekeep/cache";

/// A loaded cache: what the last run stored, and what this run has produced.
#[derive(Debug, Default)]
pub struct Store {
    entries: BTreeMap<CacheKey, Entry>,
}

impl Store {
    /// An empty store, for a run with caching disabled.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load the cache under a project root, or an empty store if there is nothing usable.
    #[must_use]
    pub fn load(project_root: &Path) -> Self {
        let Ok(bytes) = std::fs::read(Self::path_for(project_root)) else {
            return Self::default();
        };
        Self::decode(&bytes).unwrap_or_default()
    }

    /// The entry for a key, if this cache has one.
    #[must_use]
    pub fn get(&self, key: &CacheKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// Record an entry for this run.
    pub fn insert(&mut self, key: CacheKey, entry: Entry) {
        self.entries.insert(key, entry);
    }

    /// Every key held, in order.
    ///
    /// For tooling and tests that need to reach an entry without recomputing its key.
    pub fn keys(&self) -> impl Iterator<Item = &CacheKey> {
        self.entries.keys()
    }

    /// How many entries are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether anything is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Write the cache under a project root, replacing whatever was there.
    ///
    /// Silent on failure by design: an unwritable `.lanekeep` directory means the next run
    /// is cold, which is not something to interrupt this run over.
    pub fn save(&self, project_root: &Path) {
        let path = Self::path_for(project_root);
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }

        // Written beside the target and renamed over it. A rename within a directory is
        // atomic, so a reader either sees the whole previous cache or the whole new one —
        // never a half-written file. Writing in place would leave a truncated cache behind
        // if the process died mid-write, and every subsequent run would read it.
        let temporary = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&temporary, self.encode()).is_err() {
            let _ = std::fs::remove_file(&temporary);
            return;
        }
        if std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
    }

    /// Where the cache file sits for a project.
    #[must_use]
    pub fn path_for(project_root: &Path) -> PathBuf {
        project_root.join(CACHE_PATH)
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());

        // `BTreeMap` iteration is key-ordered, so the same set of entries always produces
        // byte-identical output. A cache file that churned on every run would show up as a
        // spurious diff for anyone who commits it, and would defeat content-addressed
        // storage of the cache itself.
        let mut payload = Vec::new();
        for (key, entry) in &self.entries {
            out.extend_from_slice(key.as_bytes());
            payload.clear();
            entry.encode(&mut payload);
            out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            out.extend_from_slice(&payload);
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut at = 0usize;

        let magic = bytes.get(at..at + MAGIC.len())?;
        if magic != MAGIC {
            return None;
        }
        at += MAGIC.len();

        let version = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        if version != FORMAT_VERSION {
            // Not an error: a cache written by a different build is simply not ours. The
            // version is in the key too, so this is belt and braces — but a file whose
            // layout changed must not be parsed with today's reader at all.
            return None;
        }
        at += 4;

        let count = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
        at += 8;

        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let key: [u8; 32] = bytes.get(at..at + 32)?.try_into().ok()?;
            at += 32;

            let len = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
            at += 8;
            let len = usize::try_from(len).ok()?;

            let payload = bytes.get(at..at.checked_add(len)?)?;
            at += len;

            // One unreadable entry discards the file rather than the entry. A cache that
            // silently held some entries and dropped others would make a stale result
            // depend on which byte was damaged.
            entries.insert(CacheKey::from_bytes(key), Entry::decode(payload)?);
        }

        (at == bytes.len()).then_some(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use lanekeep_core::tracked::TrackedRead;
    use lanekeep_core::{FilePath, Location, Position, Severity, Violation};

    use super::*;

    struct Project {
        dir: PathBuf,
    }

    impl Project {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lanekeep-store-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creates dir");
            Self { dir }
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn key(seed: u8) -> CacheKey {
        CacheKey::from_bytes([seed; 32])
    }

    fn entry(line: u32) -> Entry {
        Entry {
            violations: vec![Violation {
                rule_id: "local/a".parse().expect("valid id"),
                location: Location::new(FilePath::new("src/a.ts"), Position::new(line, 1)),
                message: "a message".to_owned(),
                remediation: "a remediation".to_owned(),
                severity: Severity::Error,
            }],
            facts: Vec::new(),
            dependencies: vec![TrackedRead::absent(FilePath::new("tsconfig.json"))],
            suppressions: Vec::new(),
        }
    }

    #[test]
    fn a_saved_cache_loads_back() {
        let project = Project::new("round-trip");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.insert(key(2), entry(20));
        store.save(&project.dir);

        let loaded = Store::load(&project.dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(&key(1)), Some(&entry(10)));
        assert_eq!(loaded.get(&key(2)), Some(&entry(20)));
    }

    #[test]
    fn loading_from_nothing_gives_an_empty_store() {
        let project = Project::new("absent");
        assert!(Store::load(&project.dir).is_empty());
    }

    #[test]
    fn a_corrupt_file_gives_an_empty_store() {
        // The disposability requirement: garbage means recompute, never an error.
        let project = Project::new("corrupt");
        let path = Store::path_for(&project.dir);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates dir");
        std::fs::write(&path, b"not a cache file at all").expect("writes");

        assert!(Store::load(&project.dir).is_empty());
    }

    #[test]
    fn a_truncated_file_gives_an_empty_store() {
        let project = Project::new("truncated");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.save(&project.dir);

        let path = Store::path_for(&project.dir);
        let bytes = std::fs::read(&path).expect("reads");
        for cut in 0..bytes.len() {
            std::fs::write(&path, &bytes[..cut]).expect("writes");
            assert!(
                Store::load(&project.dir).is_empty(),
                "a {cut}-byte prefix loaded as a cache"
            );
        }
    }

    #[test]
    fn a_file_from_another_format_version_is_ignored() {
        let project = Project::new("version");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.save(&project.dir);

        let path = Store::path_for(&project.dir);
        let mut bytes = std::fs::read(&path).expect("reads");
        bytes[MAGIC.len()] = bytes[MAGIC.len()].wrapping_add(1);
        std::fs::write(&path, &bytes).expect("writes");

        assert!(Store::load(&project.dir).is_empty());
    }

    #[test]
    fn one_damaged_entry_discards_the_whole_file() {
        // Otherwise which results survive would depend on which byte was damaged, and a
        // stale entry could outlive the run that should have replaced it.
        let project = Project::new("damaged");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.insert(key(2), entry(20));
        store.save(&project.dir);

        let path = Store::path_for(&project.dir);
        let mut bytes = std::fs::read(&path).expect("reads");
        let last = bytes.len() - 1;
        // The final byte of the last entry is its dependency's presence flag; 9 is neither
        // present nor absent.
        bytes[last] = 9;
        std::fs::write(&path, &bytes).expect("writes");

        assert!(Store::load(&project.dir).is_empty());
    }

    #[test]
    fn saving_the_same_entries_produces_identical_bytes() {
        // A cache file that churned on every run would show as a spurious diff for anyone
        // who commits it, and would defeat content-addressed storage of the cache itself.
        let project = Project::new("stable");

        let mut one = Store::empty();
        one.insert(key(2), entry(20));
        one.insert(key(1), entry(10));
        one.save(&project.dir);
        let first = std::fs::read(Store::path_for(&project.dir)).expect("reads");

        let mut other = Store::empty();
        other.insert(key(1), entry(10));
        other.insert(key(2), entry(20));
        other.save(&project.dir);
        let second = std::fs::read(Store::path_for(&project.dir)).expect("reads");

        assert_eq!(first, second, "insertion order leaked into the file");
    }

    #[test]
    fn saving_replaces_rather_than_appends() {
        let project = Project::new("replace");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.save(&project.dir);

        let mut replacement = Store::empty();
        replacement.insert(key(2), entry(20));
        replacement.save(&project.dir);

        let loaded = Store::load(&project.dir);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get(&key(1)).is_none());
    }

    #[test]
    fn saving_leaves_no_temporary_behind() {
        let project = Project::new("no-temp");
        let mut store = Store::empty();
        store.insert(key(1), entry(10));
        store.save(&project.dir);

        let dir = Store::path_for(&project.dir);
        let parent = dir.parent().expect("has a parent");
        let leftovers: Vec<String> = std::fs::read_dir(parent)
            .expect("reads dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    fn saving_into_an_unwritable_place_is_silent() {
        // A cache that could fail a run would be a liability. There is deliberately no
        // error to observe here — only that the call returns and the run continues.
        let store = Store::empty();
        store.save(Path::new("/definitely/not/a/writable/place"));
    }
}
