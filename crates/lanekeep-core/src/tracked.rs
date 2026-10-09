//! Tracked effects: what a rule read that was not the file it was checking.
//!
//! `ctx.readFile` makes a file's result depend on files other than itself, which breaks the
//! assumption a per-file cache is built on. Purity is therefore replaced by **tracked
//! effects**: every read is recorded, and a cache hit requires every recorded dependency to
//! still hash identically. This is the standard build-system answer, and it is what lets a
//! rule cross-reference other files without giving up incrementality.
//!
//! # Absence is a dependency too
//!
//! A rule that asks whether `tsconfig.json` exists and is told no has depended on that
//! answer just as much as one that read it. If the file later appears, the cached result is
//! stale — so a miss is recorded with a `None` hash rather than not recorded at all.
//!
//! Getting this wrong produces a cache that is correct on every test anyone thinks to write
//! and wrong on the one case that matters: adding a file makes no difference until something
//! unrelated invalidates the entry.
//!
//! # And a refusal is a third answer, not a spelling of absence
//!
//! A path that resolved out of the root through a symlink was refused unread. Recorded as
//! absent, a validator asks the filesystem whether it is still absent, follows the link, finds
//! the file and invalidates — every run, forever, for every importer of a pnpm-linked package.
//! See [`ReadOutcome::Refused`].
//!
//! # A listing is a second kind of question about a path
//!
//! `ctx.listDir` asks what a directory holds rather than what a file holds, and its answers are
//! two more outcomes: [`ReadOutcome::Listed`], carrying a digest of the entries, and
//! [`ReadOutcome::Unlisted`], when nothing listable was there. They are their own variants rather
//! than a reuse of `Found` and `Absent`, because the validator has to ask the filesystem the
//! *same* question the rule asked: an absent *file* rechecked against a path that has since
//! become a directory still reads as absent, so a listing recorded as `Absent` would hold after
//! the directory appeared — a stale answer with nothing to say so. A refusal is shared between
//! the two kinds, because it is decided by `canonicalize` alone and rechecked identically.

use crate::location::FilePath;

/// A blake3 digest of a file's bytes.
///
/// Kept as bytes rather than hex, because it is compared far more often than it is printed —
/// once per dependency per cached file per run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wrap a digest.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    /// Hex, for diagnostics and cache dumps.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// What a tracked read found.
///
/// Three states rather than an `Option<ContentHash>`, because a refusal is not an absence.
/// A path refused for resolving out of the root through a symlink — `node_modules/pkg` as
/// pnpm links it — was recorded as absent, and a validator checking absence reads
/// `root.join(path)` and *follows the link*: the file is there, so the dependency reads as
/// "appeared", every importer of a store-linked package misses the cache on every run, and
/// the validator has read bytes outside the project root to decide it. Recording the refusal
/// as itself is what lets the validator reproduce the decision that was actually made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReadOutcome {
    /// The file was read, and hashed to this.
    Found(ContentHash),
    /// Nothing readable was there.
    ///
    /// A recorded answer, not a missing record. See the module documentation.
    Absent,
    /// It resolved outside the root through a symlink, so it was refused unread.
    ///
    /// Still a dependency: the answer that rested on the refusal has to be reconsidered the
    /// day the path becomes a real in-root file.
    Refused,
    /// A directory was listed, and its rendered entries hashed to this.
    ///
    /// See `files::listing_hash` for what is hashed: names in order, a directory's carrying
    /// its trailing `/`, so a file replaced by a same-named directory is a change.
    Listed(ContentHash),
    /// A listing found no directory there — nothing at the path, or a file.
    ///
    /// Distinct from [`Self::Absent`] for the reason the module documentation gives: the two are
    /// rechecked by asking two different questions.
    Unlisted,
}

/// One file a rule reached for while checking another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackedRead {
    /// Path relative to the project root.
    pub path: FilePath,

    /// What was found there.
    pub outcome: ReadOutcome,
}

impl TrackedRead {
    /// A read that found a file.
    #[must_use]
    pub const fn found(path: FilePath, hash: ContentHash) -> Self {
        Self {
            path,
            outcome: ReadOutcome::Found(hash),
        }
    }

    /// A read that found nothing.
    #[must_use]
    pub const fn absent(path: FilePath) -> Self {
        Self {
            path,
            outcome: ReadOutcome::Absent,
        }
    }

    /// A read that was refused because the path left the root through a symlink.
    #[must_use]
    pub const fn refused(path: FilePath) -> Self {
        Self {
            path,
            outcome: ReadOutcome::Refused,
        }
    }

    /// A listing that found a directory, whose entries hashed to `hash`.
    #[must_use]
    pub const fn listed(path: FilePath, hash: ContentHash) -> Self {
        Self {
            path,
            outcome: ReadOutcome::Listed(hash),
        }
    }

    /// A listing that found no directory.
    #[must_use]
    pub const fn unlisted(path: FilePath) -> Self {
        Self {
            path,
            outcome: ReadOutcome::Unlisted,
        }
    }

    /// The hash of what was read, or `None` for every answer that read nothing.
    ///
    /// For a listing, the digest of its entries.
    ///
    /// For the callers that only ever asked "same bytes as before?". Anything deciding what
    /// to *do* about a dependency has to match on [`Self::outcome`] instead: absence and
    /// refusal are checked against the filesystem in two different ways.
    #[must_use]
    pub const fn hash(&self) -> Option<ContentHash> {
        match self.outcome {
            ReadOutcome::Found(hash) | ReadOutcome::Listed(hash) => Some(hash),
            ReadOutcome::Absent | ReadOutcome::Refused | ReadOutcome::Unlisted => None,
        }
    }
}

/// Sort dependencies into the order a cache entry stores them in.
///
/// By path. The order a rule happened to read files in is not interesting and would make two
/// entries for identical dependency sets compare unequal — which would look like a cache
/// miss with no cause a reader could find.
pub fn sort(reads: &mut [TrackedRead]) {
    reads.sort_by(|a, b| a.path.cmp(&b.path));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(seed: u8) -> ContentHash {
        ContentHash::new([seed; 32])
    }

    #[test]
    fn a_hash_renders_as_hex() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x0a;
        bytes[31] = 0xff;
        let rendered = ContentHash::new(bytes).to_string();
        assert_eq!(rendered.len(), 64, "a blake3 digest is 64 hex characters");
        assert!(rendered.starts_with("0a"), "{rendered}");
        assert!(rendered.ends_with("ff"), "{rendered}");
    }

    #[test]
    fn an_absent_read_is_still_a_dependency() {
        // The case that makes a cache wrong rather than merely cold: a rule asked whether a
        // file existed, was told no, and that answer has to be invalidated when it appears.
        let read = TrackedRead::absent(FilePath::new("tsconfig.json"));
        assert_eq!(read.hash(), None);
        assert_ne!(
            read,
            TrackedRead::found(FilePath::new("tsconfig.json"), hash(0)),
            "absence and presence must not compare equal"
        );
    }

    #[test]
    fn a_refusal_is_neither_absence_nor_a_reading() {
        // The three have to be distinguishable on the entry, because the validator checks each
        // of them against the filesystem in a different way: absence by looking for the file,
        // a refusal by re-resolving the path under the same confinement, a reading by hashing.
        let path = FilePath::new("node_modules/pkg/index.d.ts");
        let refused = TrackedRead::refused(path.clone());
        assert_eq!(refused.outcome, ReadOutcome::Refused);
        assert_eq!(refused.hash(), None, "nothing was read");
        assert_ne!(refused, TrackedRead::absent(path.clone()));
        assert_ne!(refused, TrackedRead::found(path, hash(0)));
    }

    #[test]
    fn a_listing_is_neither_a_reading_nor_an_absence() {
        // The same digest found as a file and as a listing are two different answers, and a
        // directory that was not there is not a file that was not there: each is rechecked by
        // asking the filesystem the question that was originally asked.
        let path = FilePath::new("src");
        assert_ne!(
            TrackedRead::listed(path.clone(), hash(0)),
            TrackedRead::found(path.clone(), hash(0))
        );
        assert_ne!(
            TrackedRead::unlisted(path.clone()),
            TrackedRead::absent(path.clone())
        );
        assert_eq!(TrackedRead::listed(path.clone(), hash(3)).hash(), Some(hash(3)));
        assert_eq!(TrackedRead::unlisted(path).hash(), None);
    }

    #[test]
    fn dependencies_sort_by_path() {
        let mut reads = vec![
            TrackedRead::found(FilePath::new("b.json"), hash(1)),
            TrackedRead::absent(FilePath::new("a.json")),
            TrackedRead::found(FilePath::new("c.json"), hash(2)),
        ];
        sort(&mut reads);
        assert_eq!(
            reads.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["a.json", "b.json", "c.json"]
        );
    }

    #[test]
    fn the_order_reads_happened_in_does_not_survive() {
        // Two entries covering the same dependencies have to compare equal whichever order
        // the rule reached for them, or an entry would miss for a reason nothing explains.
        let one = {
            let mut reads = vec![
                TrackedRead::found(FilePath::new("b.json"), hash(1)),
                TrackedRead::found(FilePath::new("a.json"), hash(2)),
            ];
            sort(&mut reads);
            reads
        };
        let other = {
            let mut reads = vec![
                TrackedRead::found(FilePath::new("a.json"), hash(2)),
                TrackedRead::found(FilePath::new("b.json"), hash(1)),
            ];
            sort(&mut reads);
            reads
        };
        assert_eq!(one, other);
    }
}
