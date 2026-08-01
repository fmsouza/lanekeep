//! What decides whether a cached result may be used.
//!
//! The key is a hash over everything a file's result depends on except its dependencies,
//! which are checked separately (§8.2). Everything listed here is an input because leaving
//! any of it out produces the same class of bug: a result computed by code, configuration or
//! a grammar that no longer exists, served as though it were current.
//!
//! Over-invalidation costs a recompute. Under-invalidation reports the wrong answer and
//! gives no sign that it did. The two are not symmetric, which is why anything doubtful goes
//! in the key.

use lanekeep_core::ContentHash;

/// The on-disk format's version.
///
/// Bumped when the encoding changes. Because it feeds the key, an old file simply misses
/// rather than being misread — the cache is disposable, so a format change costs one cold
/// run and needs no migration.
pub const FORMAT_VERSION: u32 = 4;

/// Everything about a run that every file's key shares.
///
/// Computed once and reused, because hashing the ruleset and config per file would repeat
/// identical work thousands of times per run.
#[derive(Debug, Clone)]
pub struct RunKey {
    prefix: blake3::Hasher,
}

impl RunKey {
    /// Fold in everything that is constant for a run.
    ///
    /// `engine_version` should be major.minor only: a patch release by definition changes no
    /// behavior a rule can observe, and invalidating every cache on it would make patch
    /// upgrades expensive for no benefit.
    #[must_use]
    pub fn new(
        engine_version: &str,
        host_api_version: u32,
        ruleset_hash: &[u8],
        config_hash: &[u8],
        grammars: &[GrammarKey],
    ) -> Self {
        let mut prefix = blake3::Hasher::new();

        // Length-prefixed, so `("ab", "c")` and `("a", "bc")` cannot hash alike. Without
        // this two genuinely different runs could share a key, which is the one failure
        // this whole module exists to prevent.
        write_field(&mut prefix, b"lanekeep-cache");
        write_field(&mut prefix, &FORMAT_VERSION.to_le_bytes());
        write_field(&mut prefix, engine_version.as_bytes());
        write_field(&mut prefix, &host_api_version.to_le_bytes());
        write_field(&mut prefix, ruleset_hash);
        write_field(&mut prefix, config_hash);

        // Every registered grammar, not the one a given file happens to use. A grammar bump
        // changes node shapes and therefore what a query matches; folding the whole set in
        // means a bump anywhere invalidates everything, which over-invalidates by exactly
        // the files that use the other languages — a recompute, against the alternative of
        // reasoning per file about which grammars a file's rules could have involved.
        write_field(&mut prefix, &(grammars.len() as u64).to_le_bytes());
        for grammar in grammars {
            write_field(&mut prefix, grammar.id.as_bytes());
            write_field(&mut prefix, &grammar.abi.to_le_bytes());
        }

        Self { prefix }
    }

    /// The key for a file whose result depends on the date.
    ///
    /// Only for a file carrying an expiring suppression. Folding the date into every key
    /// would invalidate the whole cache daily for the sake of the handful of files that
    /// have one — and leaving it out entirely would serve an expired suppression as though
    /// it were still in force, which is the one thing an expiry exists to prevent.
    #[must_use]
    pub fn for_dated_file(&self, path: &str, content: &ContentHash, today: &str) -> CacheKey {
        let mut hasher = self.prefix.clone();
        write_field(&mut hasher, path.as_bytes());
        write_field(&mut hasher, content.as_bytes());
        write_field(&mut hasher, today.as_bytes());
        CacheKey(*hasher.finalize().as_bytes())
    }

    /// The key for one file.
    ///
    /// The **path** is an input as well as the content, because path gates make results
    /// path-sensitive — a moved file with identical bytes is not a hit.
    #[must_use]
    pub fn for_file(&self, path: &str, content: &ContentHash) -> CacheKey {
        let mut hasher = self.prefix.clone();
        write_field(&mut hasher, path.as_bytes());
        write_field(&mut hasher, content.as_bytes());
        CacheKey(*hasher.finalize().as_bytes())
    }
}

/// The grammar a file was parsed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarKey {
    /// The language's identifier.
    pub id: String,
    /// The tree-sitter ABI version the grammar was built against.
    pub abi: u32,
}

/// A cache key: what an entry is stored under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    /// Wrap raw bytes, for decoding an entry that is already on disk.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for CacheKey {
    /// The first eight hex characters, which is all a diagnostic needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Absorb a field, length-prefixed so concatenation is unambiguous.
fn write_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    // `u64` rather than `usize`, so a cache written on a 64-bit host is readable by a
    // 32-bit one — the key would otherwise differ for no reason a user could see.
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> RunKey {
        RunKey::new("0.1", 1, b"ruleset", b"config", &[grammar()])
    }

    fn grammar() -> GrammarKey {
        GrammarKey {
            id: "typescript".to_owned(),
            abi: 15,
        }
    }

    fn content(seed: u8) -> ContentHash {
        ContentHash::new([seed; 32])
    }

    fn key_of(run: &RunKey, path: &str, seed: u8) -> CacheKey {
        run.for_file(path, &content(seed))
    }

    #[test]
    fn the_same_inputs_give_the_same_key() {
        assert_eq!(key_of(&run(), "src/a.ts", 1), key_of(&run(), "src/a.ts", 1));
    }

    #[test]
    fn changing_the_content_changes_the_key() {
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&run(), "src/a.ts", 2));
    }

    #[test]
    fn moving_a_file_changes_the_key() {
        // Path gates make results path-sensitive, so identical bytes at a new path are not
        // a hit — a rule restricted to `src/**` must not have its verdict follow the file
        // into `test/**`.
        assert_ne!(
            key_of(&run(), "src/a.ts", 1),
            key_of(&run(), "test/a.ts", 1)
        );
    }

    #[test]
    fn changing_the_ruleset_changes_the_key() {
        let other = RunKey::new("0.1", 1, b"different", b"config", &[grammar()]);
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_config_changes_the_key() {
        let other = RunKey::new("0.1", 1, b"ruleset", b"different", &[grammar()]);
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_engine_version_changes_the_key() {
        let other = RunKey::new("0.2", 1, b"ruleset", b"config", &[grammar()]);
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_host_api_version_changes_the_key() {
        // A result computed without a host function is not a valid result for a run that
        // has it: the rule could not have called something that did not exist.
        let other = RunKey::new("0.1", 2, b"ruleset", b"config", &[grammar()]);
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn adding_a_grammar_changes_the_key() {
        let more = RunKey::new(
            "0.1",
            1,
            b"ruleset",
            b"config",
            &[
                grammar(),
                GrammarKey {
                    id: "javascript".to_owned(),
                    abi: 15,
                },
            ],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&more, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_grammar_abi_changes_the_key() {
        // A grammar bump changes node shapes and therefore what a query matches.
        let bumped = RunKey::new(
            "0.1",
            1,
            b"ruleset",
            b"config",
            &[GrammarKey {
                id: "typescript".to_owned(),
                abi: 16,
            }],
        );
        assert_ne!(
            key_of(&run(), "src/a.ts", 1),
            key_of(&bumped, "src/a.ts", 1)
        );
    }

    #[test]
    fn changing_the_language_changes_the_key() {
        let other = RunKey::new(
            "0.1",
            1,
            b"ruleset",
            b"config",
            &[GrammarKey {
                id: "javascript".to_owned(),
                abi: 15,
            }],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn fields_cannot_run_together() {
        // The reason every field is length-prefixed. Without it `("ab", "c")` and
        // `("a", "bc")` hash alike, and two genuinely different runs share a key — which is
        // the one failure mode a cache must not have.
        let one = RunKey::new("0.1", 1, b"ab", b"c", &[grammar()]);
        let other = RunKey::new("0.1", 1, b"a", b"bc", &[grammar()]);
        assert_ne!(key_of(&one, "src/a.ts", 1), key_of(&other, "src/a.ts", 1));

        // And on the per-file side: a path and a content digest must not be able to run
        // together into the same byte sequence as a different pair.
        assert_ne!(
            run().for_file("src/ab.ts", &content(1)),
            run().for_file("src/a", &content(1))
        );
    }

    #[test]
    fn a_dated_key_changes_with_the_date() {
        // An expiring suppression served from a cache written yesterday would never expire.
        let content = content(1);
        assert_ne!(
            run().for_dated_file("src/a.ts", &content, "2026-08-01"),
            run().for_dated_file("src/a.ts", &content, "2026-08-02")
        );
    }

    #[test]
    fn a_dated_key_differs_from_an_undated_one() {
        let content = content(1);
        assert_ne!(
            run().for_file("src/a.ts", &content),
            run().for_dated_file("src/a.ts", &content, "2026-08-01")
        );
    }

    #[test]
    fn a_key_renders_short_for_diagnostics() {
        let rendered = key_of(&run(), "src/a.ts", 1).to_string();
        assert_eq!(rendered.len(), 8);
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
