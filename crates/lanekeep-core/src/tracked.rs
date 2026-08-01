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

/// One file a rule reached for while checking another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackedRead {
    /// Path relative to the project root.
    pub path: FilePath,

    /// Hash of what was read, or `None` if nothing was there.
    ///
    /// `None` is a recorded answer, not a missing record. See the module documentation.
    pub hash: Option<ContentHash>,
}

impl TrackedRead {
    /// A read that found a file.
    #[must_use]
    pub const fn found(path: FilePath, hash: ContentHash) -> Self {
        Self {
            path,
            hash: Some(hash),
        }
    }

    /// A read that found nothing.
    #[must_use]
    pub const fn absent(path: FilePath) -> Self {
        Self { path, hash: None }
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
        assert_eq!(read.hash, None);
        assert_ne!(
            read,
            TrackedRead::found(FilePath::new("tsconfig.json"), hash(0)),
            "absence and presence must not compare equal"
        );
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
