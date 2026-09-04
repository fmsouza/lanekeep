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
    ///
    /// # The three host-side inputs, and why they are separate
    ///
    /// `host_api_hash` is **what a rule may reach**: the `ctx` surface QuickJS installs and
    /// the WIT world a component is linked against, plus anything the host binds beside that
    /// world. A result computed by a build where a host function did not exist is not a valid
    /// result for a build where it does — the rule could not have called it, so its verdict
    /// was reached without evidence it would have used. It is a hash rather than a
    /// hand-maintained number because a number has to be remembered: `lanekeep-js`'s
    /// `HOST_API_VERSION` says so in its own documentation, and nothing detects a missed bump.
    ///
    /// `wasm_compile_env_hash` is **how a component is compiled**, which is a different
    /// question with the same failure mode. A precompiled `.cwasm` records the tunables it was
    /// built under and `wasmtime` refuses one that disagrees, so those tunables decide whether
    /// a component runs at all — and the ones that survive that check still decide what the
    /// guest computes, because they move Cranelift's codegen. Callers derive it from
    /// `wasmtime`'s own compatibility hash rather than by listing fields, so a `wasmtime`
    /// upgrade that moves a field nobody here has heard of still invalidates.
    ///
    /// **A compilation environment and not a runtime**, which is why it is not called one.
    /// Settings that live entirely host-side are deliberately outside it: the memory ceiling
    /// is enforced by a `ResourceLimiter` the compiled code knows nothing about, and the epoch
    /// tick interval only changes *when* a breach is noticed. Both are budgets rather than
    /// inputs, and neither belongs in a key. If a host-side setting ever does change a result,
    /// it needs its own field rather than a quiet widening of this one.
    ///
    /// They are two fields rather than one because they answer to different owners — the
    /// trust boundary and the compiler — and because a test that can only move both at once
    /// cannot tell which of them the key actually covers.
    ///
    /// `analysis_hash` is **what the host analyses compute**, which is a third question with
    /// the same failure mode as the two above. A type oracle's operator table, its shadow
    /// checks and its recursion bound decide what a rule was told; a result computed by an
    /// oracle that no longer exists is not a valid result for a run that has a different one.
    ///
    /// Separate from `host_api_hash` deliberately. That field answers what a rule may *reach*
    /// — whether `ctx.types` exists at all — and this one answers what it *says*. Folding them
    /// would leave a test unable to tell which of the two the key covers, which is the reason
    /// this module already keeps the host surface and the compilation environment apart.
    ///
    /// # All three are digests, and that is the caller's promise rather than this encoding's
    ///
    /// Every field here is length-prefixed, so nothing depends on the three being any
    /// particular width. But all three are 32-byte `blake3` digests in practice, which is
    /// worth knowing because it is what makes a *caller* mixing them up the only realistic way
    /// to get this group wrong — three same-shaped `&[u8]` arguments in a row, which no
    /// signature can tell apart — and this is public API, so a future caller could pass
    /// something else.
    #[must_use]
    pub fn new(
        engine_version: &str,
        host_api_hash: &[u8],
        wasm_compile_env_hash: &[u8],
        analysis_hash: &[u8],
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
        write_field(&mut prefix, host_api_hash);
        write_field(&mut prefix, wasm_compile_env_hash);
        write_field(&mut prefix, analysis_hash);
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
            write_field(&mut prefix, &grammar.digest);
        }

        Self { prefix }
    }

    /// The key for a file whose result depends on the date.
    ///
    /// Two ways a file becomes date-dependent, and the engine chooses this key on either:
    /// an expiring suppression in the file's bytes, or a rule that read `ctx.today` while
    /// checking it. Folding the date into every key instead would invalidate the whole
    /// cache daily for the sake of the handful of files that depend on it — and leaving it
    /// out entirely would serve yesterday's answer: an expired suppression as though it
    /// were still in force, a date comparison frozen at whenever the cache was written.
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
    /// A digest of the shape the grammar exposes: its ABI, its node kinds and its fields.
    ///
    /// Replaces the bare ABI version this carried. An ABI bump moves the digest, because the
    /// ABI is one of its inputs — and so does a grammar regeneration at an unchanged ABI,
    /// which the ABI alone could not see. Keeping both would be two fields that cannot move
    /// independently, which is the shape this module's own documentation argues against: no
    /// test could then tell which of the two the key covers.
    pub digest: [u8; 32],
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
        RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        )
    }

    fn grammar() -> GrammarKey {
        GrammarKey {
            id: "typescript".to_owned(),
            digest: [15; 32],
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
        let other = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"different",
            b"config",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_config_changes_the_key() {
        let other = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"different",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_engine_version_changes_the_key() {
        let other = RunKey::new(
            "0.2",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_host_api_hash_changes_the_key() {
        // A result computed without a host function is not a valid result for a run that
        // has it: the rule could not have called something that did not exist.
        //
        // Two different byte slices rather than two different numbers, which is the whole of
        // the change: a number is hand-maintained and a content hash is not.
        let other = RunKey::new(
            "0.1",
            b"host-api-with-one-more-function",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_wasm_compile_env_hash_changes_the_key() {
        // A precompiled component records the tunables it was compiled under, and an engine
        // configured differently cannot load it. The ones that do load still decide what the
        // guest computes — a bounds check elided or emitted is a codegen difference — so a
        // result computed under one compilation environment is not a result for another.
        let other = RunKey::new(
            "0.1",
            b"host-api",
            b"a-different-compilation-environment",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    /// The oracle's identity is its own term.
    ///
    /// Its own field rather than folded into `host_api_hash`, because that one answers what a
    /// rule may *reach* while this answers what the answers *are* — the same distinction that
    /// keeps `wasm_compile_env_hash` separate, and for the same stated reason: a test that can
    /// only move both at once cannot tell which of them the key actually covers.
    #[test]
    fn changing_the_analysis_hash_changes_the_key() {
        let other = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"different-oracle",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn the_two_host_fields_are_not_interchangeable() {
        // Two adjacent byte-slice parameters is the shape a caller swaps by accident, and
        // every "this input moves the key" test passes just as well against a swapped pair.
        // This is the one that does not: it hashes the same two values in the other order.
        let swapped = RunKey::new(
            "0.1",
            b"runtime",
            b"host-api",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        assert_ne!(
            key_of(&run(), "src/a.ts", 1),
            key_of(&swapped, "src/a.ts", 1)
        );
    }

    #[test]
    fn adding_a_grammar_changes_the_key() {
        let more = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[
                grammar(),
                GrammarKey {
                    id: "javascript".to_owned(),
                    digest: [15; 32],
                },
            ],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&more, "src/a.ts", 1));
    }

    #[test]
    fn changing_the_grammar_digest_changes_the_key() {
        // A grammar change moves node kinds, field names or the parse table, and therefore
        // what a query matches. The ABI version this field used to hold is folded into the
        // digest rather than sitting beside it: two fields that can only ever move together
        // leave no test able to say which of them the key covers.
        let bumped = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[GrammarKey {
                id: "typescript".to_owned(),
                digest: [9; 32],
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
            b"host-api",
            b"runtime",
            b"analysis",
            b"ruleset",
            b"config",
            &[GrammarKey {
                id: "javascript".to_owned(),
                digest: [15; 32],
            }],
        );
        assert_ne!(key_of(&run(), "src/a.ts", 1), key_of(&other, "src/a.ts", 1));
    }

    #[test]
    fn fields_cannot_run_together() {
        // The reason every field is length-prefixed. Without it `("ab", "c")` and
        // `("a", "bc")` hash alike, and two genuinely different runs share a key — which is
        // the one failure mode a cache must not have.
        let one = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"ab",
            b"c",
            &[grammar()],
        );
        let other = RunKey::new(
            "0.1",
            b"host-api",
            b"runtime",
            b"analysis",
            b"a",
            b"bc",
            &[grammar()],
        );
        assert_ne!(key_of(&one, "src/a.ts", 1), key_of(&other, "src/a.ts", 1));

        // And the same on the pair this change added, which are the two fields most likely
        // to be built by concatenating something.
        let one = RunKey::new(
            "0.1",
            b"ab",
            b"c",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
        let other = RunKey::new(
            "0.1",
            b"a",
            b"bc",
            b"analysis",
            b"ruleset",
            b"config",
            &[grammar()],
        );
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
