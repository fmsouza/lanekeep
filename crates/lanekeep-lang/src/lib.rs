//! Language trait and registry for lanekeep.
//!
//! The `Language` trait and the registry mapping file extensions onto grammars.
//!
//! It also owns `binding`: the shape of a resolved binding, and the convention deciding
//! which import a rule's `resolvesToImport`/`isImportedFrom` counts as a match. That
//! convention lives here rather than in either rule-execution engine because both call
//! it, and two copies would drift into answering plausibly and differently for the same
//! file — which no test on either side would catch.
//!
//! This abstraction exists before it has a second implementor on purpose. Retrofitting it
//! after a second language arrives is the expensive version of the same work.

pub mod binding;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

/// A language's stable identifier, as written in a rule's `language` field.
///
/// Deliberately not an enum. An enum would have to live in this crate and name every
/// language, so adding one would mean editing the abstraction rather than adding an
/// implementor — exactly the coupling the trait exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(&'static str);

impl LanguageId {
    /// Declare an identifier. Called by language implementations.
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    /// The identifier as it appears in configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A language lanekeep can parse.
///
/// `Send + Sync` because the walker runs files across rayon workers, and every worker needs
/// the grammar.
///
/// Binding resolution — the light semantic layer behind the import-resolution host
/// functions — is deliberately not a method here yet. Its signature depends on the tree
/// and file context types, which do not exist. Adding a method to a trait with no external
/// implementors is cheap; committing to a half-designed signature is not.
pub trait Language: Send + Sync {
    /// Stable identifier, as written in a rule's `language` field.
    fn id(&self) -> LanguageId;

    /// Identifier resolution for this language, when it has any.
    ///
    /// Returns `None` for a language with no resolver yet, which is honest rather than a
    /// placeholder: a rule asking about bindings in such a language gets nothing back
    /// instead of a confidently wrong answer.
    fn resolver(&self) -> Option<Arc<dyn binding::BindingResolver>> {
        None
    }

    /// Extensions this language claims, without the leading dot, lowercase.
    ///
    /// Two languages must not claim the same extension; the registry rejects that at
    /// registration rather than picking a winner.
    fn extensions(&self) -> &'static [&'static str];

    /// The tree-sitter grammar.
    fn grammar(&self) -> tree_sitter::Language;

    /// The grammar's ABI version.
    ///
    /// This is a cache key input. A grammar bump changes node shapes and therefore query
    /// results, so an entry computed under a different ABI is not a valid entry — it would
    /// serve results derived from a tree that no longer exists.
    ///
    /// Read from the grammar rather than written down, or it stops tracking the thing it
    /// exists to track the first time someone forgets to update it. Note that bundled
    /// grammars do not share an ABI — TypeScript and JavaScript currently differ — which
    /// is why this is per-language rather than one global constant.
    fn grammar_abi(&self) -> usize {
        self.grammar().abi_version()
    }

    /// What this language's own analysis code *is*, as a digest of the sources that decide
    /// an answer.
    ///
    /// A cache key input, and a different question from [`Self::grammar_abi`] beside it: that
    /// one says what the parse tree looks like, this one says what this crate concludes about
    /// it. A language's [`binding::BindingResolver`] decides where a name was declared, which
    /// is what `ctx.bindingKind` and `ctx.resolvesToImport` answer with and what the type
    /// oracle reads — so a result computed by a resolver that no longer exists is not a valid
    /// result for a run that has a different one.
    ///
    /// The gap this closes was not theoretical. `lanekeep_types::oracle_identity` was the
    /// whole of the key's analysis term, and it digests `crates/lanekeep-types/src/` alone;
    /// the scope list deciding which nodes carry type parameters lives in
    /// `lanekeep-lang-js`, and correcting it moved what the oracle answered while every hash
    /// stayed identical.
    ///
    /// Defaulted rather than required, matching [`Self::resolver`] and [`Self::grammar_abi`]:
    /// this is published API and a required method would break every external implementor.
    /// The gap that leaves — a language crate with a resolver and no build script — is closed
    /// by a test in `lanekeep-languages` rather than by the compiler.
    ///
    /// Implementors derive this rather than writing it down. See any language crate's
    /// `build.rs`.
    fn analysis_identity(&self) -> [u8; 32] {
        [0; 32]
    }
}

/// Why a language could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// Two languages claim the same identifier.
    #[error("language `{0}` is already registered")]
    DuplicateId(String),

    /// Two languages claim the same file extension.
    #[error(
        "extension `.{extension}` is claimed by both `{existing}` and `{incoming}`: \
         a file cannot belong to two languages"
    )]
    DuplicateExtension {
        /// The contested extension.
        extension: String,
        /// The language that claimed it first.
        existing: String,
        /// The language that tried to claim it second.
        incoming: String,
    },

    /// A language declared an extension that cannot match anything.
    #[error("language `{language}` declared invalid extension `{extension}`: {reason}")]
    InvalidExtension {
        /// The language at fault.
        language: String,
        /// The extension as declared.
        extension: String,
        /// What is wrong with it.
        reason: &'static str,
    },
}

/// Which languages are available, and which files belong to them.
#[derive(Clone, Default)]
pub struct LanguageRegistry {
    by_id: BTreeMap<&'static str, Arc<dyn Language>>,
    by_extension: BTreeMap<&'static str, Arc<dyn Language>>,
}

/// Hand-written because `Arc<dyn Language>` is not `Debug` — trait objects would have to
/// require it, which is a demand on every implementor for the sake of one impl here. The
/// keys are the useful part anyway: what is registered, and what it claims.
impl fmt::Debug for LanguageRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LanguageRegistry")
            .field("by_id", &self.by_id.keys().collect::<Vec<_>>())
            .field(
                "by_extension",
                &self.by_extension.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl LanguageRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a language.
    ///
    /// # Errors
    ///
    /// Fails when the identifier or any extension is already claimed, or when an extension
    /// is malformed. Rejecting rather than overwriting is deliberate: a registry that
    /// silently let the last registration win would make which language parses a `.ts`
    /// file depend on registration order, and that order is not part of any contract.
    ///
    /// A failed registration changes nothing — validation of every extension completes
    /// before any is claimed.
    pub fn register(&mut self, language: Arc<dyn Language>) -> Result<(), RegistryError> {
        let id = language.id().as_str();

        if self.by_id.contains_key(id) {
            return Err(RegistryError::DuplicateId(id.to_owned()));
        }

        for extension in language.extensions() {
            let invalid = |reason: &'static str| RegistryError::InvalidExtension {
                language: id.to_owned(),
                extension: (*extension).to_owned(),
                reason,
            };

            if extension.is_empty() {
                return Err(invalid("must not be empty"));
            }
            if extension.starts_with('.') {
                return Err(invalid("must not include the leading dot"));
            }
            if extension.chars().any(|c| c.is_ascii_uppercase()) {
                return Err(invalid(
                    "must be lowercase; lookup lowercases the path's extension",
                ));
            }
            if let Some(existing) = self.by_extension.get(extension) {
                return Err(RegistryError::DuplicateExtension {
                    extension: (*extension).to_owned(),
                    existing: existing.id().as_str().to_owned(),
                    incoming: id.to_owned(),
                });
            }
        }

        for extension in language.extensions() {
            self.by_extension.insert(extension, Arc::clone(&language));
        }
        self.by_id.insert(id, language);
        Ok(())
    }

    /// Look up a language by its identifier.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<&Arc<dyn Language>> {
        self.by_id.get(id)
    }

    /// Which language, if any, parses this path.
    ///
    /// The extension is lowercased before lookup, so a file named `Button.TSX` is still
    /// TSX. Without this, whether a file gets checked would depend on how it was typed.
    #[must_use]
    pub fn for_path(&self, path: impl AsRef<Path>) -> Option<&Arc<dyn Language>> {
        let extension = path.as_ref().extension()?.to_str()?.to_ascii_lowercase();
        self.by_extension.get(extension.as_str())
    }

    /// Every registered language, ordered by identifier.
    pub fn languages(&self) -> impl Iterator<Item = &Arc<dyn Language>> {
        self.by_id.values()
    }

    /// Every extension any language claims, ordered.
    pub fn extensions(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.by_extension.keys().copied()
    }

    /// How many languages are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no language is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Decode the 64-character lowercase hex a build script emitted into 32 bytes.
///
/// Every language crate's `build.rs` writes its digest as hex, because that is what a
/// `cargo:rustc-env` value can carry. One decoder rather than one per crate: they would be
/// identical, and a copy that drifts would produce a digest that is stable, wrong, and
/// indistinguishable from a correct one.
///
/// Total rather than fallible. The only inputs are constants this workspace's own build
/// scripts wrote, so there is no caller input to reject and nothing a caller could do about a
/// malformed one; a digit outside `0-9a-f` reads as zero, and a string shorter than 64
/// characters leaves the remaining bytes zero.
#[must_use]
pub fn decode_hex32(hex: &str) -> [u8; 32] {
    let bytes = hex.as_bytes();
    let mut out = [0_u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let hi = index * 2;
        let lo = hi + 1;
        if lo >= bytes.len() {
            break;
        }
        *slot = (hex_value(bytes[hi]) << 4) | hex_value(bytes[lo]);
    }
    out
}

/// One lowercase hex digit as a nibble, or zero for anything else.
const fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in whose grammar is never exercised. The registry's job is bookkeeping, and
    /// testing it through a real language would couple these tests to whichever one exists.
    struct Fake {
        id: LanguageId,
        extensions: &'static [&'static str],
    }

    impl Language for Fake {
        fn id(&self) -> LanguageId {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.extensions
        }
        fn grammar(&self) -> tree_sitter::Language {
            unreachable!("registry tests never touch the grammar")
        }
        fn grammar_abi(&self) -> usize {
            0
        }
    }

    fn fake(id: &'static str, extensions: &'static [&'static str]) -> Arc<dyn Language> {
        Arc::new(Fake {
            id: LanguageId::new(id),
            extensions,
        })
    }

    fn registry(languages: &[Arc<dyn Language>]) -> LanguageRegistry {
        let mut registry = LanguageRegistry::new();
        for language in languages {
            registry.register(Arc::clone(language)).expect("registers");
        }
        registry
    }

    #[test]
    fn finds_a_language_by_id() {
        let registry = registry(&[fake("alpha", &["a"])]);
        assert_eq!(
            registry.by_id("alpha").expect("present").id().as_str(),
            "alpha"
        );
        assert!(registry.by_id("missing").is_none());
    }

    #[test]
    fn finds_a_language_by_path() {
        let registry = registry(&[fake("alpha", &["a", "aa"]), fake("beta", &["b"])]);

        assert_eq!(
            registry.for_path("src/x.a").expect("matches").id().as_str(),
            "alpha"
        );
        assert_eq!(
            registry
                .for_path("src/x.aa")
                .expect("matches")
                .id()
                .as_str(),
            "alpha"
        );
        assert_eq!(
            registry.for_path("src/x.b").expect("matches").id().as_str(),
            "beta"
        );
        assert!(registry.for_path("src/x.zzz").is_none());
        assert!(registry.for_path("src/noextension").is_none());
    }

    #[test]
    fn extension_lookup_ignores_case() {
        // A case-insensitive filesystem lets `Button.TSX` and `Button.tsx` name the same
        // file. Whether it gets checked must not depend on how it was typed.
        let registry = registry(&[fake("alpha", &["a"])]);
        assert!(registry.for_path("src/x.A").is_some());
        assert!(registry.for_path("src/x.a").is_some());
    }

    #[test]
    fn rejects_a_duplicate_id() {
        let mut registry = registry(&[fake("alpha", &["a"])]);
        let err = registry
            .register(fake("alpha", &["z"]))
            .expect_err("duplicate id");
        assert_eq!(err, RegistryError::DuplicateId("alpha".to_owned()));
    }

    #[test]
    fn rejects_a_contested_extension() {
        // The important one. Letting the last registration win would make which language
        // parses a file depend on registration order — an order nothing guarantees, and a
        // difference that would surface as results changing for no visible reason.
        let mut registry = registry(&[fake("alpha", &["a"])]);
        let err = registry
            .register(fake("beta", &["a"]))
            .expect_err("contested extension");

        match err {
            RegistryError::DuplicateExtension {
                extension,
                existing,
                incoming,
            } => {
                assert_eq!(extension, "a");
                assert_eq!(existing, "alpha");
                assert_eq!(incoming, "beta");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn a_rejected_registration_leaves_no_trace() {
        // Partial registration would be worse than rejection: the language would be
        // unreachable by id while still owning whichever extensions were processed before
        // the conflict.
        let mut registry = registry(&[fake("alpha", &["a"])]);
        let _ = registry.register(fake("beta", &["b", "a", "c"]));

        assert!(registry.by_id("beta").is_none());
        assert!(
            registry.for_path("x.b").is_none(),
            "b must not have been claimed"
        );
        assert!(
            registry.for_path("x.c").is_none(),
            "c must not have been claimed"
        );
        assert_eq!(
            registry.for_path("x.a").expect("still alpha").id().as_str(),
            "alpha"
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn rejects_malformed_extensions() {
        let mut registry = LanguageRegistry::new();

        // A leading dot would never match, because `Path::extension` strips it.
        assert!(matches!(
            registry.register(fake("dotted", &[".a"])),
            Err(RegistryError::InvalidExtension { .. })
        ));
        // Uppercase would never match either, since lookup lowercases first.
        assert!(matches!(
            registry.register(fake("shouty", &["A"])),
            Err(RegistryError::InvalidExtension { .. })
        ));
        assert!(matches!(
            registry.register(fake("empty", &[""])),
            Err(RegistryError::InvalidExtension { .. })
        ));
        assert!(registry.is_empty());
    }

    #[test]
    fn iteration_order_is_stable() {
        // Anything derived from registry order — a `--help` listing, an error naming the
        // valid languages — must not reorder between runs.
        let registry = registry(&[
            fake("zeta", &["z"]),
            fake("alpha", &["a"]),
            fake("mu", &["m"]),
        ]);

        let ids: Vec<&str> = registry.languages().map(|l| l.id().as_str()).collect();
        assert_eq!(ids, ["alpha", "mu", "zeta"]);
        assert_eq!(registry.extensions().collect::<Vec<_>>(), ["a", "m", "z"]);
    }

    #[test]
    fn an_empty_registry_matches_nothing() {
        let registry = LanguageRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.for_path("src/x.ts").is_none());
        assert!(registry.by_id("typescript").is_none());
    }

    #[test]
    fn hex_decodes_to_the_bytes_it_spells() {
        let mut expected = [0_u8; 32];
        expected[0] = 0x0a;
        expected[1] = 0xff;
        expected[31] = 0x10;
        let hex = format!("0aff{}10", "00".repeat(29));
        assert_eq!(decode_hex32(&hex), expected);
    }

    /// A short or malformed string leaves zeros rather than panicking, which is what makes
    /// this safe to call on a constant no caller supplied.
    #[test]
    fn a_malformed_hex_string_decodes_to_zeros() {
        assert_eq!(decode_hex32(""), [0; 32]);
        assert_eq!(decode_hex32("zz"), [0; 32]);
    }
}
