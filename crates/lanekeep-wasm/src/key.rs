//! The two values a run folds into its cache key before a component decides anything.
//!
//! Both exist for the same reason and neither is a version number. `docs/architecture.md`
//! §8.1 lists what a cached result depends on; leaving any of it out serves an answer
//! computed under conditions that no longer hold, with no sign that it did. Over-invalidating
//! costs a recompute, and the two are not symmetric.
//!
//! # [`host_api_hash`] — what a rule may reach
//!
//! `lanekeep-js`'s `HOST_API_VERSION` is a hand-maintained `u32`, and its own documentation
//! says plainly that nothing detects a missed bump: a `ctx` function added without one serves
//! results computed by a build where the function did not exist. A content hash removes the
//! step a person has to remember. The WIT file is the single source every binding in this
//! project is generated from, so hashing its bytes says exactly "the host surface changed"
//! without anybody deciding that it did.
//!
//! The `package lanekeep:host@0.1.0;` semver stays in the file, for error messages and for
//! the component model's own conventions. It is not what the cache keys on, because a
//! hand-maintained semver has precisely the failure mode the `u32` has.
//!
//! **The WIT bytes are not the whole surface.** An interface the host binds *beside* the
//! declared world is reachable by a component exactly as a declared one is, and the world
//! file says nothing about it. That is not hypothetical: TinyGo's runtime imports
//! `wasi:random/random` unconditionally, and the host must **bind** it rather than decline it,
//! since a declined import means the component never instantiates at all. Whatever fixed byte
//! cycle it is bound to then decides a Go rule's map iteration order, and therefore which
//! violation it reports — measured 2026-08-05 with TinyGo 0.41.1, where a fixed source pinned
//! the order across twelve runs and a different source pinned a different one. Change the
//! cycle with the world, the component and the config all identical and every cache-key input
//! is unchanged while the answer moves. [`EXTERNAL_BINDINGS`] is where that value is declared,
//! and it is folded in here so the declaration is already an input on the day one is added.
//!
//! # [`compile_env_hash`] — how a component is compiled
//!
//! A precompiled `.cwasm` records the tunables it was built under, and `wasmtime` refuses to
//! load one that disagrees with the host — "Module was compiled with a memory reservation of
//! '4294967296' but '1048576' is expected for the host". So the compilation environment
//! decides whether a component runs at all; and among the settings that survive that check,
//! `crate::runtime::MEMORY_RESERVATION` and `crate::runtime::MEMORY_GUARD_SIZE` together
//! decide whether Cranelift elides bounds checks, which is a 1.6× on guest compute and a
//! different machine-code shape. Neither is a knob, and both are cache-key inputs.
//!
//! **This does not list the fields, and that is the point.** `check_tunables` in
//! `wasmtime-47.0.3/src/engine/serialization.rs` performs twenty-six comparisons; the three
//! constants `crate::runtime` declares are three of them, and the other twenty-three are
//! covered by the `wasmtime` version and the target triple *only if the feature set is pinned
//! too* — `component-model-async` moves `concurrency_support`, `rr` moves `recording`, and
//! `custom-virtual-memory` moves `memory_reservation` and `memory_init_cow`, any of which
//! Cargo's feature unification can move without the version moving. A hand-written list would
//! have to be kept in step with all of that. [`wasmtime::Engine::precompile_compatibility_hash`]
//! is `wasmtime`'s own answer to the same question — it hashes the target triple, the
//! compiler and ISA flags, the whole `Tunables` struct, the enabled wasm features and the
//! `wasmtime` crate version — and it is read off the [`wasmtime::Engine`] built from
//! `crate::runtime::config`, which is the one configuration every engine in this crate is
//! built from. So the constants reach the key from where they are defined rather than from a
//! copy, and a `wasmtime` upgrade that moves a field nobody here has heard of still
//! invalidates.
//!
//! Two consequences worth stating rather than discovering:
//!
//! - **The value is host-specific by construction**, because the target triple and the ISA
//!   flags are in it. Two machines will not agree on it, which is correct — a `.cwasm` built
//!   on one does not load on the other — and harmless, because a cache is per checkout.
//! - **`std::hash::Hash` is not a stable serialization format.** A `rustc` upgrade that
//!   changes a `Hash` implementation, or a `wasmtime` patch that adds a field, moves this
//!   value without anything about lanekeep changing. That is over-invalidation: one cold run,
//!   once. The alternative — a hand-maintained list that silently stops covering a field — is
//!   the error this module exists to avoid, and it is the one that is not recoverable.

use std::sync::OnceLock;

use crate::error::WasmError;
use crate::runtime::engine;

/// The WIT source every binding in this project is generated from.
///
/// The same file `crate::bindings`' `bindgen!` reads, by the same path — `bindgen!` is given
/// the `wit` directory rather than this file, and
/// `tests::the_wit_directory_holds_exactly_the_file_this_module_hashes` is what keeps the two
/// from drifting when a second `.wit` file arrives.
const WORLD_WIT: &str = include_str!("../wit/world.wit");

/// An interface the host binds that the world in `wit/world.wit` does not declare.
///
/// Two strings, because two different things can change a rule's answer. `interface` is
/// *what* the guest can reach; `behavior` is what pins the answer it gets — the seed of a
/// random stream, the sink stdout is wired to. A host that swapped one fixed entropy source
/// for another would leave `interface` alone and change every Go rule's map iteration order,
/// so a declaration carrying only the name would be a cache-key input that does not move when
/// the thing it describes does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalBinding {
    interface: &'static str,
    behavior: &'static str,
}

impl ExternalBinding {
    /// Declare an interface bound outside the world, and what fixes its answers.
    ///
    /// `behavior` must describe the value, not the intent: "a fixed 64-byte cycle, all
    /// zeroes" is a declaration that moves when the cycle does, and "deterministic" is not.
    #[must_use]
    pub const fn declare(interface: &'static str, behavior: &'static str) -> Self {
        Self {
            interface,
            behavior,
        }
    }

    /// The interface name, as the component model spells it.
    #[must_use]
    pub const fn interface(&self) -> &'static str {
        self.interface
    }

    /// What fixes the answers the host gives through it.
    #[must_use]
    pub const fn behavior(&self) -> &'static str {
        self.behavior
    }
}

/// Every interface this build binds beside the declared world.
///
/// **Empty, and that is a statement rather than a placeholder.** A Rust rule built for
/// `wasm32-unknown-unknown` imports exactly `lanekeep:host/types@0.1.0`, so nothing needs to
/// be bound beside the world today, and `crate::runtime::RuleSet::new` binds nothing else.
///
/// The Go authoring lane is what changes that: TinyGo's runtime imports `wasi:random/random`,
/// `wasi:io/streams`, `wasi:io/error` and `wasi:cli/stdout` whether the rule uses them or not,
/// and those must be bound rather than declined. **Whatever binds one belongs here in the same
/// change**, because this list is folded into [`host_api_hash`] and the thing it describes is
/// not otherwise anywhere in the cache key. `crate::runtime::RuleSet::linker_mut` asks for a
/// declaration at the call site so that binding without writing one down is not a thing that
/// can be done by forgetting.
pub const EXTERNAL_BINDINGS: &[ExternalBinding] = &[];

/// What a rule may reach: the declared world, and what is bound beside it.
///
/// See the module documentation for why this is a hash of the WIT source rather than the
/// `0.1.0` in its package declaration.
#[must_use]
pub fn host_api_hash() -> [u8; 32] {
    hash_host_api(WORLD_WIT, EXTERNAL_BINDINGS)
}

/// The fold, separated from its inputs so a test can vary them.
///
/// A cache-key input that only ever gets hashed with one value is an input nothing has
/// checked. Every field is length-prefixed for the reason `lanekeep_cache::key` gives: two
/// different declarations must not be able to concatenate into one byte sequence.
fn hash_host_api(wit: &str, external: &[ExternalBinding]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    field(&mut hasher, b"lanekeep-host-api-v1");

    // Line endings normalized, because a checkout is not an API change. Git has no
    // `.gitattributes` here to force LF, so a Windows checkout with `core.autocrlf` on
    // produces the same world with different bytes, and the whole claim of this hash is that
    // it moves when the *surface* moves.
    field(&mut hasher, wit.replace("\r\n", "\n").as_bytes());

    field(&mut hasher, b"external");
    field(&mut hasher, &(external.len() as u64).to_le_bytes());
    for binding in external {
        field(&mut hasher, binding.interface.as_bytes());
        field(&mut hasher, binding.behavior.as_bytes());
    }

    *hasher.finalize().as_bytes()
}

/// How a component is compiled: everything `wasmtime` checks a precompiled artifact against.
///
/// Memoized, because it builds a throwaway [`wasmtime::Engine`] to read the answer off and a
/// `--watch` loop asks once per iteration otherwise. The value cannot change within a process.
///
/// # Errors
///
/// Returns [`WasmError::Engine`] when `wasmtime` cannot realize `crate::runtime::config` on
/// this host, which means a broken build rather than anything about a rule. Deliberately not
/// swallowed into a constant: a run that cannot build an engine cannot key a result against
/// one either, and guessing a value here would put a cache entry under a key that describes
/// nothing.
pub fn compile_env_hash() -> Result<[u8; 32], WasmError> {
    static CACHED: OnceLock<Result<[u8; 32], String>> = OnceLock::new();

    CACHED
        .get_or_init(|| {
            let engine = engine().map_err(|e| e.to_string())?;
            Ok(hash_engine(&engine))
        })
        .clone()
        .map_err(WasmError::Engine)
}

/// The fold, separated from the shipped engine so a test can vary the configuration.
///
/// Same reason [`hash_host_api`] is separate: three of the twenty-six fields `wasmtime`
/// compares are constants this crate sets deliberately, and "the key covers the memory
/// reservation" is a claim that needs a second reservation to be worth anything.
fn hash_engine(engine: &wasmtime::Engine) -> [u8; 32] {
    let mut hasher = Blake3Hasher(blake3::Hasher::new());
    std::hash::Hash::hash(&engine.precompile_compatibility_hash(), &mut hasher);
    *hasher.0.finalize().as_bytes()
}

/// A [`std::hash::Hasher`] that writes into blake3.
///
/// `wasmtime` hands out its compatibility descriptor as `impl std::hash::Hash`, which can only
/// be read through a `Hasher`; blake3 is not one. This adapter is the whole of the bridge —
/// every `Hash` implementation reaches this crate's digest through [`Self::write`], and
/// nothing calls [`Self::finish`], because the digest is taken from the blake3 hasher
/// directly.
struct Blake3Hasher(blake3::Hasher);

impl std::hash::Hasher for Blake3Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The first eight bytes of the digest so far.
    ///
    /// Required by the trait and unused by this crate. It is implemented honestly rather than
    /// left to return zero, because a `Hash` implementation is allowed to call it — and one
    /// that did would otherwise be told every input hashes alike.
    fn finish(&self) -> u64 {
        let digest = self.0.finalize();
        let mut head = [0u8; 8];
        head.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(head)
    }
}

/// Absorb a field, length-prefixed so concatenation is unambiguous.
///
/// `u64` rather than `usize`, so the value does not depend on the width of the machine that
/// computed it — the same reasoning `lanekeep_cache::key::write_field` carries.
fn field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroes() -> ExternalBinding {
        ExternalBinding::declare(
            "wasi:random/random@0.2.0",
            "a fixed 64-byte cycle of zeroes",
        )
    }

    fn ones() -> ExternalBinding {
        ExternalBinding::declare("wasi:random/random@0.2.0", "a fixed 64-byte cycle of ones")
    }

    #[test]
    fn the_host_api_hash_is_the_same_twice() {
        assert_eq!(host_api_hash(), host_api_hash());
    }

    #[test]
    fn one_more_byte_of_wit_changes_the_host_api_hash() {
        let appended = format!("{WORLD_WIT}\n");
        assert_ne!(
            hash_host_api(WORLD_WIT, EXTERNAL_BINDINGS),
            hash_host_api(&appended, EXTERNAL_BINDINGS)
        );
    }

    #[test]
    fn a_carriage_return_does_not_change_the_host_api_hash() {
        // A Windows checkout is not a host API change. This is the one difference in the WIT
        // bytes that must *not* move the key.
        let crlf = WORLD_WIT.replace('\n', "\r\n");
        assert_ne!(crlf, WORLD_WIT, "the fixture has to actually differ");
        assert_eq!(
            hash_host_api(WORLD_WIT, EXTERNAL_BINDINGS),
            hash_host_api(&crlf, EXTERNAL_BINDINGS)
        );
    }

    #[test]
    fn binding_an_interface_outside_the_world_changes_the_host_api_hash() {
        assert_ne!(
            hash_host_api(WORLD_WIT, &[]),
            hash_host_api(WORLD_WIT, &[zeroes()])
        );
    }

    #[test]
    fn changing_a_bound_entropy_source_changes_the_host_api_hash() {
        // The one this exists for. The interface is identical, the world is identical, the
        // component is identical — and a Go rule's map iteration order, and therefore which
        // violation it reports, is not.
        assert_ne!(
            hash_host_api(WORLD_WIT, &[zeroes()]),
            hash_host_api(WORLD_WIT, &[ones()])
        );
    }

    #[test]
    fn a_declaration_cannot_run_together_with_the_next_one() {
        // Length prefixes, on the pair most likely to be written as one string. Without them
        // `("ab", "c")` and `("a", "bc")` are one byte sequence.
        assert_ne!(
            hash_host_api(WORLD_WIT, &[ExternalBinding::declare("ab", "c")]),
            hash_host_api(WORLD_WIT, &[ExternalBinding::declare("a", "bc")])
        );
    }

    #[test]
    fn the_wit_directory_holds_exactly_the_file_this_module_hashes() {
        // `bindgen!` is pointed at the directory and this module at one file inside it, so a
        // second `.wit` would reach the bindings and not the cache key — a host surface that
        // could change with every input to the key unmoved. There is no `include_str!` over a
        // directory, so this is the tripwire instead.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
        let mut found: Vec<String> = std::fs::read_dir(&dir)
            .expect("the crate ships its own wit directory")
            .map(|entry| {
                entry
                    .expect("the directory is readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        found.sort();

        assert_eq!(
            found,
            vec!["world.wit".to_owned()],
            "`host_api_hash` reads only wit/world.wit — a second file has to be added to it \
             as well, or the host surface it declares is outside the cache key"
        );
    }

    #[test]
    fn the_compile_environment_hash_is_the_same_twice() {
        let first = compile_env_hash().expect("the shipped configuration builds");
        let second = compile_env_hash().expect("the shipped configuration builds");
        assert_eq!(first, second);
    }

    /// Hash an engine built from the shipped configuration with one thing changed.
    fn hash_with(change: impl FnOnce(&mut wasmtime::Config)) -> [u8; 32] {
        let mut config = crate::runtime::config();
        change(&mut config);
        let engine = wasmtime::Engine::new(&config).expect("the varied configuration builds");
        hash_engine(&engine)
    }

    #[test]
    fn the_shipped_configuration_is_what_is_hashed() {
        // The whole "read the constants from where they are defined" property in one
        // assertion: the value a run keys on is the one taken off an engine built from
        // `runtime::config`, so the reservation, the guard and epoch interruption reach the
        // cache key from their `const` declarations and not from a copy.
        assert_eq!(
            compile_env_hash().expect("the shipped configuration builds"),
            hash_with(|_| {})
        );
    }

    #[test]
    fn a_different_memory_reservation_changes_the_compile_environment_hash() {
        assert_ne!(
            hash_with(|_| {}),
            hash_with(|config| {
                config.memory_reservation(16 * 1024 * 1024);
            })
        );
    }

    #[test]
    fn a_different_memory_guard_size_changes_the_compile_environment_hash() {
        assert_ne!(
            hash_with(|_| {}),
            hash_with(|config| {
                config.memory_guard_size(0);
            })
        );
    }

    #[test]
    fn turning_off_epoch_interruption_changes_the_compile_environment_hash() {
        // The third artifact-validity constant, and the one the decision record's condition 3
        // originally left out.
        assert_ne!(
            hash_with(|_| {}),
            hash_with(|config| {
                config.epoch_interruption(false);
            })
        );
    }

    #[test]
    fn a_different_wasmtime_version_changes_the_compile_environment_hash() {
        // A `wasmtime` upgrade cannot be staged in-process, so this moves the field the
        // version travels in. `ModuleVersionStrategy::WasmtimeVersion` hashes
        // `env!("CARGO_PKG_VERSION")` of the `wasmtime` crate
        // (`wasmtime-47.0.3/src/config.rs:105-113`), and a custom strategy hashes the string
        // given instead — so a value reaching the digest through this setting is the version
        // reaching it too.
        assert_ne!(
            hash_with(|_| {}),
            hash_with(|config| {
                config
                    .module_version(wasmtime::ModuleVersionStrategy::Custom(
                        "not-the-shipped-wasmtime".to_owned(),
                    ))
                    .expect("a custom module version is accepted");
            })
        );
    }

    #[test]
    fn the_compilers_own_identity_changes_the_compile_environment_hash() {
        // Where the target triple lives, reached by the one lever that moves in this build.
        //
        // `HashedEngineCompileEnv` hashes the triple, the compiler's shared flags and its ISA
        // flags in one block (`wasmtime-47.0.3/src/compile/code_builder.rs:848-865`). The
        // triple itself cannot be varied here: `Config::target` with anything but this host's
        // fails with "Support for this target is disabled", because that needs wasmtime's
        // `all-arch` feature and this build compiles for one architecture on purpose. An
        // optimization level is a Cranelift shared flag, so moving it exercises the same
        // block — which is evidence about that block rather than about the triple, and is
        // written down as such rather than dressed up as a triple test.
        assert_ne!(
            hash_with(|_| {}),
            hash_with(|config| {
                config.cranelift_opt_level(wasmtime::OptLevel::None);
            })
        );
    }

    #[test]
    fn the_compile_environment_hash_is_not_empty() {
        // A `Hasher` adapter that dropped its writes would produce blake3 of nothing, which
        // is a perfectly stable value and covers no input at all.
        let empty = *blake3::Hasher::new().finalize().as_bytes();
        assert_ne!(
            compile_env_hash().expect("the shipped configuration builds"),
            empty
        );
    }
}
