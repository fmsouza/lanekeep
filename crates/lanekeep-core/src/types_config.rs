//! Which type oracle a project wants, and how to reach it.
//!
//! # Why this lives in `lanekeep-core` rather than in `lanekeep-config`
//!
//! It is configuration data and every instinct puts it there. The dependency graph forbids
//! it: `lanekeep-config` depends on `lanekeep-js`, which depends on `lanekeep-types`, so a
//! provider in `lanekeep-types` that took a `lanekeep_config::TypesConfig` would close a
//! cycle. `lanekeep-core` is the one crate both sides already depend on. `lanekeep-config`
//! re-exports these two names, so `lanekeep_config::TypesConfig` still resolves and there is
//! exactly one type rather than one per crate that needs it.

use blake3::Hasher;

/// Which oracle answers `ctx.types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TypesProvider {
    /// lanekeep's own bounded oracle. Always available, needs no toolchain.
    #[default]
    Builtin,
    /// The project's own `typescript` package, driven through a sidecar process.
    Tsc,
}

impl TypesProvider {
    /// The name a config writes, which is the name a refusal prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Tsc => "tsc",
        }
    }

    /// The provider that name denotes, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "builtin" => Some(Self::Builtin),
            "tsc" => Some(Self::Tsc),
            _ => None,
        }
    }

    /// Every provider, for a refusal that lists what is valid.
    ///
    /// Built from the variants rather than from a second hand-maintained table, for the
    /// reason `Capability::all` gives: a name is listed exactly when there is a variant for
    /// it and cannot silently disagree with what [`TypesProvider::parse`] recognizes.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Builtin, Self::Tsc]
    }
}

/// The `types` block, with defaults filled in.
///
/// `command` and `typescript` mean nothing under [`TypesProvider::Builtin`] and are still
/// carried and still hashed: a config that sets them and then switches provider has said two
/// different things, and a cache key that could not tell them apart would serve the first
/// run's answer for the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesConfig {
    /// Which oracle answers.
    pub provider: TypesProvider,
    /// How to launch the sidecar. `tsc` only.
    pub command: Vec<String>,
    /// The `typescript` package to load, as a specifier `createRequire` resolves from the
    /// project root. `tsc` only.
    pub typescript: String,
}

impl Default for TypesConfig {
    fn default() -> Self {
        Self {
            provider: TypesProvider::Builtin,
            command: vec!["node".to_owned()],
            typescript: "./node_modules/typescript".to_owned(),
        }
    }
}

impl TypesConfig {
    /// This block as bytes, for a hash.
    ///
    /// Length-prefixed throughout, which is the same framing `hash_ruleset` and
    /// `fold_analysis` use and for the same reason: `["ab", "c"]` and `["a", "bc"]`
    /// concatenate to identical bytes, so two genuinely different configurations would share
    /// one provider identity.
    ///
    /// A fold rather than canonical JSON because `lanekeep-core` has no `serde_json` outside
    /// its dev-dependencies, and because every other identity in this tree is a fold.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_field(&mut out, self.provider.as_str().as_bytes());
        push_field(&mut out, &(self.command.len() as u64).to_le_bytes());
        for argument in &self.command {
            push_field(&mut out, argument.as_bytes());
        }
        push_field(&mut out, self.typescript.as_bytes());
        out
    }

    /// A stable 32-byte digest of the block, for a provider identity.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(b"lanekeep-types-config-v1");
        hasher.update(&self.canonical_bytes());
        *hasher.finalize().as_bytes()
    }
}

fn push_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_provider_is_the_builtin_one() {
        let config = TypesConfig::default();
        assert_eq!(config.provider, TypesProvider::Builtin);
        assert_eq!(config.command, vec!["node".to_owned()]);
        assert_eq!(config.typescript, "./node_modules/typescript");
    }

    #[test]
    fn canonical_bytes_separate_every_field() {
        // The reason each field is length-prefixed. Unprefixed, a command of `["ab", "c"]`
        // and one of `["a", "bc"]` concatenate identically, and two different configurations
        // would produce one provider identity — the one failure a cache key must not have.
        let one = TypesConfig {
            provider: TypesProvider::Tsc,
            command: vec!["ab".to_owned(), "c".to_owned()],
            typescript: "./t".to_owned(),
        };
        let two = TypesConfig {
            command: vec!["a".to_owned(), "bc".to_owned()],
            ..one.clone()
        };
        assert_ne!(one.canonical_bytes(), two.canonical_bytes());
    }

    #[test]
    fn the_provider_choice_reaches_the_canonical_bytes() {
        let tsc = TypesConfig {
            provider: TypesProvider::Tsc,
            ..TypesConfig::default()
        };
        assert_ne!(
            TypesConfig::default().canonical_bytes(),
            tsc.canonical_bytes()
        );
    }

    #[test]
    fn a_provider_name_round_trips_through_its_own_spelling() {
        for provider in [TypesProvider::Builtin, TypesProvider::Tsc] {
            assert_eq!(TypesProvider::parse(provider.as_str()), Some(provider));
        }
        assert_eq!(TypesProvider::parse("tsserver"), None);
    }
}
