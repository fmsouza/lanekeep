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
    lanekeep_lang_python::register_all(registry)
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
        assert_eq!(ids, ["javascript", "python", "tsx", "typescript"]);
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
}
