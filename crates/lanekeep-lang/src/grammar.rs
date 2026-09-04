//! What a grammar *is*, as a digest of the shape it exposes.
//!
//! A cache key input. What a tree-sitter query matches depends on the node kinds and fields
//! the grammar has, so a result computed against a different grammar is not a valid result
//! for this run — the same failure `oracle_identity` and the host API hash exist to prevent,
//! one layer down.
//!
//! # Not the grammar's declared version
//!
//! That was the first thing tried. `tree_sitter::Language::metadata()` returns the version a
//! grammar was generated with and it is `None` on any grammar built for an ABI below 15.
//! Measured over the six languages registered here: `go`, `javascript`, `python` and `rust`
//! are ABI 15 and answer `Some`; `typescript` and `tsx` are ABI 14 and answer `None`, as does
//! `name()`. So the two grammars this project reads most could not be identified that way at
//! all.
//!
//! # What this does not catch
//!
//! A regeneration that rearranges the parse table while preserving every node-kind name,
//! every field name and all three counts. Hashing the grammar's bytes would catch it, and the
//! tree-sitter Rust API does not expose them. This is strictly better than the bare ABI
//! version it replaces, under which no change within one ABI moved anything at all.
//!
//! One of the five terms carries no information for `typescript` and `tsx`, which is exactly
//! the pair this module exists for. `ts_language_supertypes` is gated on
//! `abi_version >= LANGUAGE_VERSION_WITH_RESERVED_WORDS` and returns length zero below it, so
//! the `supertypes` list is always empty for those two.
//!
//! The per-node `supertype` flag beside it is **not** gated, and it would be easy to assume it
//! was. `ts_language_symbol_type` reads `symbol_metadata[symbol].supertype` with no version
//! check at all, and tree-sitter-typescript 0.23.2 — `LANGUAGE_VERSION 14` — sets
//! `.supertype = true` on seven symbols, each with `.visible = false` and `.named = true`,
//! which is the combination that makes `node_kind_is_supertype` answer `true`. So that flag is
//! live on ABI 14 and carries real information.
//!
//! Strictly better than the bare ABI version this replaces, then, and not every term here is
//! informative for every grammar.

use tree_sitter::Language;

/// One node kind, as the grammar describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeKind {
    /// The kind's name, as a query writes it.
    pub name: String,
    /// Whether the kind is named rather than an anonymous token.
    pub named: bool,
    /// Whether the kind appears in the tree a query sees.
    pub visible: bool,
    /// Whether the kind is a supertype standing for several others.
    pub supertype: bool,
}

/// The shape a grammar exposes, read out of it once.
///
/// Separated from [`Self::digest`] so a test can move one field at a time. A
/// `tree_sitter::Language` cannot be constructed synthetically, so a digest written only
/// against real grammars could assert that six of them differ and never that any particular
/// input reaches the hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarShape {
    /// The tree-sitter ABI the grammar was built against.
    pub abi: usize,
    /// How many parse states the table has.
    pub parse_states: usize,
    /// Every supertype id, in the order the grammar lists them.
    pub supertypes: Vec<u16>,
    /// Every node kind, by id.
    pub kinds: Vec<NodeKind>,
    /// Every field name, by id. Index 0 is the absent field and reads empty.
    pub fields: Vec<String>,
}

impl GrammarShape {
    /// Read a grammar's shape.
    #[must_use]
    pub fn of(language: &Language) -> Self {
        let kinds = (0..language.node_kind_count())
            .map(|id| {
                // Node kind ids are `u16` in tree-sitter's C API and `node_kind_count` counts
                // them, so this cannot truncate. Saturating rather than panicking, matching
                // `counted()` below.
                let id = u16::try_from(id).unwrap_or(u16::MAX);
                NodeKind {
                    name: language.node_kind_for_id(id).unwrap_or("").to_owned(),
                    named: language.node_kind_is_named(id),
                    visible: language.node_kind_is_visible(id),
                    supertype: language.node_kind_is_supertype(id),
                }
            })
            .collect();

        // `0..=field_count` rather than `0..`: field id 0 is "no field" and answers `None`,
        // and the real ids run from 1 through the count inclusive. Reading the absent one
        // costs an empty string and keeps the indices lined up with the ids.
        let fields = (0..=language.field_count())
            .map(|id| {
                let id = u16::try_from(id).unwrap_or(u16::MAX);
                language.field_name_for_id(id).unwrap_or("").to_owned()
            })
            .collect();

        Self {
            abi: language.abi_version(),
            parse_states: language.parse_state_count(),
            supertypes: language.supertypes().to_vec(),
            kinds,
            fields,
        }
    }

    /// Fold the shape into 32 bytes.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lanekeep-grammar-v1");
        length_prefixed(&mut hasher, &counted(self.abi).to_le_bytes());
        length_prefixed(&mut hasher, &counted(self.parse_states).to_le_bytes());

        length_prefixed(&mut hasher, &counted(self.supertypes.len()).to_le_bytes());
        for supertype in &self.supertypes {
            length_prefixed(&mut hasher, &supertype.to_le_bytes());
        }

        length_prefixed(&mut hasher, &counted(self.kinds.len()).to_le_bytes());
        for kind in &self.kinds {
            length_prefixed(&mut hasher, kind.name.as_bytes());
            length_prefixed(
                &mut hasher,
                &[
                    u8::from(kind.named),
                    u8::from(kind.visible),
                    u8::from(kind.supertype),
                ],
            );
        }

        length_prefixed(&mut hasher, &counted(self.fields.len()).to_le_bytes());
        for field in &self.fields {
            length_prefixed(&mut hasher, field.as_bytes());
        }

        *hasher.finalize().as_bytes()
    }
}

/// A grammar's shape, folded into 32 bytes.
///
/// The whole of the public surface for callers that do not need the shape itself.
#[must_use]
pub fn grammar_digest(language: &Language) -> [u8; 32] {
    GrammarShape::of(language).digest()
}

/// A count as a fixed-width integer, saturating rather than panicking.
fn counted(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Length-prefixed, so two different shapes cannot fold to identical bytes.
fn length_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&counted(bytes.len()).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> GrammarShape {
        GrammarShape {
            abi: 15,
            parse_states: 1442,
            supertypes: vec![1, 2],
            kinds: vec![NodeKind {
                name: "identifier".to_owned(),
                named: true,
                visible: true,
                supertype: false,
            }],
            fields: vec![String::new(), "name".to_owned()],
        }
    }

    /// Every field moves the digest, asserted one at a time.
    ///
    /// A `tree_sitter::Language` cannot be constructed synthetically, so a digest tested only
    /// against real grammars could assert that six of them differ and never that any
    /// particular input reaches the hash. This is why the shape is a struct.
    #[test]
    fn every_field_reaches_the_digest() {
        let base = shape().digest();

        let mut abi = shape();
        abi.abi = 14;
        assert_ne!(base, abi.digest(), "abi");

        let mut states = shape();
        states.parse_states = 1443;
        assert_ne!(base, states.digest(), "parse_states");

        let mut supertypes = shape();
        supertypes.supertypes = vec![1, 3];
        assert_ne!(base, supertypes.digest(), "supertypes");

        let mut renamed = shape();
        renamed.kinds[0].name = "type_identifier".to_owned();
        assert_ne!(base, renamed.digest(), "kind name");

        let mut unnamed = shape();
        unnamed.kinds[0].named = false;
        assert_ne!(base, unnamed.digest(), "kind named flag");

        let mut hidden = shape();
        hidden.kinds[0].visible = false;
        assert_ne!(base, hidden.digest(), "kind visible flag");

        let mut supertype = shape();
        supertype.kinds[0].supertype = true;
        assert_ne!(base, supertype.digest(), "kind supertype flag");

        let mut fields = shape();
        fields.fields[1] = "value".to_owned();
        assert_ne!(base, fields.digest(), "field name");
    }

    /// The reason every part is length-prefixed.
    ///
    /// Without it a kind named `ab` beside a field named `c` folds to the same bytes as a
    /// kind named `a` beside a field named `bc`, and two genuinely different grammars share
    /// a digest.
    #[test]
    fn parts_cannot_run_together() {
        let mut one = shape();
        one.kinds[0].name = "ab".to_owned();
        one.fields = vec![String::new(), "c".to_owned()];

        let mut two = shape();
        two.kinds[0].name = "a".to_owned();
        two.fields = vec![String::new(), "bc".to_owned()];

        assert_ne!(one.digest(), two.digest());
    }

    #[test]
    fn the_same_shape_gives_the_same_digest() {
        assert_eq!(shape().digest(), shape().digest());
    }
}
