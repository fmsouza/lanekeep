//! Every language lanekeep supports, assembled in one place.
//!
//! The composition root for languages, and the only place that answers "which languages are
//! there". Both the CLI and the testkit need that answer, and neither can host it: a crate
//! below them would have to depend on the concrete language crates, which inverts the
//! dependency the `Language` trait exists to avoid.
//!
//! Assembling it twice instead would be three lines in each place, and the failure mode is
//! quiet — a rule tested against a language the CLI does not register, or the reverse.

use lanekeep_lang::{LanguageRegistry, RegistryError};

/// Register every supported language.
///
/// # Errors
///
/// Propagates [`RegistryError`] if two languages claim the same identifier or extension.
/// That is a bug here rather than a user error, which is why [`registry`] treats it as one.
pub fn register_all(registry: &mut LanguageRegistry) -> Result<(), RegistryError> {
    lanekeep_lang_js::register_all(registry)?;
    lanekeep_lang_python::register_all(registry)?;
    lanekeep_lang_go::register_all(registry)?;
    lanekeep_lang_rust::register_all(registry)
}

/// A registry holding every supported language.
///
/// # Panics
///
/// If two built-in languages conflict, which no input can cause.
#[must_use]
pub fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    #[expect(
        clippy::expect_used,
        reason = "documented above: two built-in languages claiming the same identifier or \
                  extension is a bug here, not a condition a caller can handle"
    )]
    {
        register_all(&mut registry).expect("built-in languages do not conflict");
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_language_is_registered() {
        let registry = registry();
        let mut ids: Vec<&str> = registry.languages().map(|l| l.id().as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            ["go", "javascript", "python", "rust", "tsx", "typescript"]
        );
    }

    #[test]
    fn files_resolve_to_the_language_that_parses_them() {
        let registry = registry();
        for (path, expected) in [
            ("src/a.ts", "typescript"),
            ("src/a.tsx", "tsx"),
            ("src/a.js", "javascript"),
            ("src/a.py", "python"),
            ("src/a.pyi", "python"),
        ] {
            assert_eq!(
                registry.for_path(path).expect("matches").id().as_str(),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn every_language_offers_a_resolver() {
        // A language with none is honest rather than wrong, but every language shipped so
        // far has one, and a new one arriving without would be worth noticing.
        for language in registry().languages() {
            assert!(
                language.resolver().is_some(),
                "{} has no binding resolver",
                language.id().as_str()
            );
        }
    }

    /// Every registered language carries an identity, and it is derived rather than defaulted.
    ///
    /// This test is what stands in for a required trait method. `analysis_identity` has a
    /// default of `[0; 32]`, because `Language` is published and `resolver` and `grammar_abi`
    /// both set the precedent of a defaulted method — so nothing in the type system catches a
    /// language crate that ships a resolver and no `build.rs`. This does, at the one place
    /// that knows which languages exist.
    #[test]
    fn every_registered_language_has_a_derived_analysis_identity() {
        for language in registry().languages() {
            assert_ne!(
                language.analysis_identity(),
                [0; 32],
                "{} returns the trait default, so its crate has no build script",
                language.id().as_str()
            );
        }
    }

    /// And the identity is per *crate*, not per language.
    ///
    /// `typescript`, `tsx` and `javascript` all come from `lanekeep-lang-js` and share one
    /// resolver, so they must share one identity; the other three crates contribute one each.
    /// Asserting the count rather than only "nonzero" is what would catch every language
    /// returning the same constant.
    #[test]
    fn the_analysis_identities_are_one_per_crate() {
        let mut identities: Vec<[u8; 32]> = registry()
            .languages()
            .map(|language| language.analysis_identity())
            .collect();
        assert_eq!(identities.len(), 6, "six languages are registered");
        identities.sort_unstable();
        identities.dedup();
        assert_eq!(
            identities.len(),
            4,
            "four language crates ship a resolver; js registers three of the six languages"
        );
    }

    /// Six registered grammars, six digests.
    ///
    /// The complement of the per-field test in `lanekeep-lang`: that one proves each input
    /// reaches the hash, this one proves the real grammars differ in at least one of them.
    /// Neither is enough alone — a digest that ignored every input would pass the first if it
    /// were written against a constant, and would fail here.
    #[test]
    fn every_registered_grammar_has_its_own_digest() {
        let mut digests: Vec<[u8; 32]> = registry()
            .languages()
            .map(|language| lanekeep_lang::grammar_digest(&language.grammar()))
            .collect();
        assert_eq!(digests.len(), 6, "six languages are registered");
        digests.sort_unstable();
        digests.dedup();
        assert_eq!(digests.len(), 6, "two grammars share a digest");
    }
}
