//! Where a rule's compiled form comes from, and what it is allowed to import.
//!
//! Two questions, answered in this order, before anything is instantiated: *may this run
//! this?* and *from what?* Both are decision-record conditions rather than
//! implementation detail, and both have a measured reason for being written the way they
//! are.
//!
//! # The import check is a requirement of its own, not belt-and-braces
//!
//! A wrongly-targeted component fails to instantiate today only because this host declines
//! to link WASI, and that protection disappears the moment any host links WASI for any
//! reason. Measured 2026-08-05: a `componentize-py` component built the default way imports
//! twenty-six interfaces — the declared world plus twenty-five WASI, among them
//! `wasi:clocks/wall-clock`, `wasi:random/random`, `wasi:filesystem/types` and seven
//! `wasi:sockets/*` — and on a host that links WASI it read the real wall clock
//! (`time.time()` returned `1785937477.39764` against the host's own `1785937477.392`) and
//! the host process's real environment, fifty variables. Nothing about that component is
//! detectable at instantiation on such a host. It is trivially detectable from its import
//! list, which is what [`check_imports`] reads.
//!
//! **It is a permitted set and not an equality, and that is not generality for its own
//! sake.** "The import list equals the declared world" happens to hold for Rust on
//! `wasm32-unknown-unknown` and for Python under `componentize-py --stub-wasi`, and it
//! cannot hold for Go: TinyGo's runtime imports `wasi:io/streams`, `wasi:cli/stdout` and
//! `wasi:random/random` unconditionally, so `wasm-tools component new` against a bare custom
//! world fails outright with "module requires an import interface named
//! `wasi:io/streams@0.2.0`" — the world has to `include wasi:cli/imports@0.2.0` before a
//! component exists at all, and the result imports five interfaces. An equality check would
//! reject every Go rule ever written. [`PermittedImports::allowing`] is what lets that set be
//! named without redesigning the mechanism.
//!
//! What this module does *not* build is the other half of that lane: the fixed
//! `wasi:random/random` binding a Go rule needs — bound, not declined, because declining
//! means the component never instantiates — and the sink bindings for `wasi:cli/stdout`,
//! `wasi:io/streams` and `wasi:io/error`. Those belong with Go authoring. What it owes them
//! is a check that can express their set, and nothing here hardcodes "one import".
//!
//! # Precompiled, and mapped from a file rather than from a slice
//!
//! Both halves are conditions, and each has a different reason.
//!
//! **Precompilation is about the cold budget.** Measured 2026-08-05: compiling twenty Rust
//! rule components with `Component::from_file` costs about 186 ms against about 0.74 ms to
//! load twenty precompiled ones with `Component::deserialize_file` — a factor of roughly 252,
//! and 186 ms is 23% of the 800 ms cold budget in `docs/architecture.md` §15, spent before a
//! file is read. Both are a single timed pass; a second preserved run gives 179.805 and
//! 0.693, a factor of 259, and nothing turns on which. The one-time cost is negligible:
//! `Component::serialize` costs 36.4 µs at the median against 9,183.4 µs to compile the same
//! component (n = 200 each), so the break-even against compiling is the first load. The
//! `.cwasm` is 5.43× the component — 90,224 bytes for 16,618 — which for the built-ins is
//! under a megabyte on disk.
//!
//! **Mapping is about residency.** `include_bytes!` produces a byte slice, a byte slice
//! cannot be mapped, and wasmtime builds copy-on-write memory images only from something it
//! can map: `MemoryImage::new` takes the file path only when the backing mapping reports an
//! `original_file`, which a compiled-in-process artifact never has. Measured 2026-08-05, same
//! binary, same engine configuration, same instantiation loop, slope from 20 to 100 live
//! instances: an in-process component costs 11.79 MiB per instance over a 71.7 MB floor; a
//! mapped one costs 0.48 MiB per instance, or 1.45 MiB once `check` has actually run on every
//! instance, over a 23 MB floor — roughly **8×**. And the in-process path instantiates more
//! slowly: 5.833 µs against 4.791 µs at the median, **18% slower**, with more than double the
//! standard deviation (0.967 against 0.398) and minima that do not overlap (5.333 against
//! 4.625 µs), n = 2,000 per arm. Fourteen workers holding one instance each is about 165 MiB
//! over a 71.7 MB floor in process, against about 21 MiB over a 23 MB floor mapped.
//!
//! **Two figures worth stating so nobody optimizes the wrong one.** The mapped path is 30%
//! slower to *load* — 30.2 µs against 23.3 µs. Loading happens once per run and instantiation
//! happens per component per worker, so at twenty rules and fourteen workers the mapped path
//! gives up about 0.14 ms and wins back about 0.29 ms plus the residency.
//!
//! Precompilation belongs to lanekeep's own cache directory rather than to wasmtime's `cache`
//! feature. A second cache with its own invalidation rules, keyed by something lanekeep's
//! cache key does not control, is precisely the failure that cache key exists to prevent.
//!
//! # The slice is a fallback, and never the load path
//!
//! [`LoadSource::Embedded`] is reached when there is no writable cache directory — a
//! read-only container image, a sandboxed CI step, a run with caching off — and not
//! otherwise. The failure mode it exists to prevent is a tool that stops working where it
//! used to work: today the cache directory holds *results*, and a run without one still
//! works. Under this shape it also holds *code*.
//!
//! [`ComponentLoader::embedded_loads`] is what makes "and never otherwise" checkable rather
//! than merely stated.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use wasmtime::Engine;
use wasmtime::component::Component;
use wasmtime::component::types::ComponentItem;

use crate::error::WasmError;
use crate::runtime::WasmEngine;

/// The one instance import `wit/world.wit` declares.
///
/// A component built for `wasm32-unknown-unknown` against that world imports exactly this and
/// nothing else. It is spelled here rather than derived from the world, because what has to
/// be compared against is a *name in an artifact* — deriving it from the same file the
/// artifact was built from would make the check agree with itself by construction.
pub const HOST_INTERFACE: &str = "lanekeep:host/types@0.1.0";

/// Where precompiled rule components live, relative to the project root.
///
/// Beside `.lanekeep/cache` and not inside it: the cache file is one blob rewritten whole on
/// every run, and these are content-addressed files that outlive it.
pub const COMPONENT_CACHE_PATH: &str = ".lanekeep/components";

/// The file extension wasmtime's own tooling uses for a precompiled artifact.
const ARTIFACT_EXTENSION: &str = "cwasm";

/// How many hex characters of the component digest name its artifact.
///
/// Thirty-two, so 128 bits. The digest is what makes the name change when the component
/// does; it is not a security boundary, because anything that can replace the artifact can
/// replace the component it was compiled from.
const DIGEST_CHARS: usize = 32;

/// The longest a sanitized rule name may be inside an artifact filename.
///
/// The name is a readability affordance — the digest is what identifies the artifact — so
/// truncating it costs nothing, and not truncating it lets a rule id decide how long a path
/// component gets.
const NAME_CHARS: usize = 48;

/// The instance imports a component may declare.
///
/// A *set* the caller supplies rather than a fixed list, for the reason the module
/// documentation gives: Rust and stubbed Python components import exactly the declared world,
/// and a Go component cannot, so a check spelled as equality would reject an entire authoring
/// language.
///
/// Only *instance* imports are considered. wasmtime's raw import list also carries the bare
/// `check-context` and `reduce-context` resource-type imports the component model requires
/// because those types appear in an export signature — bookkeeping rather than reachable
/// capability, and a check written as `imports().len() == 1` rejects every rule over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermittedImports {
    names: BTreeSet<String>,
}

impl PermittedImports {
    /// Exactly what `wit/world.wit` declares, and nothing else.
    ///
    /// The set every rule built for `wasm32-unknown-unknown` satisfies, and the default.
    #[must_use]
    pub fn declared_world() -> Self {
        Self {
            names: BTreeSet::from([HOST_INTERFACE.to_owned()]),
        }
    }

    /// Nothing at all.
    ///
    /// For a caller that means to name every permitted interface itself, and for asserting
    /// that the check is doing work rather than passing everything.
    #[must_use]
    pub fn none() -> Self {
        Self {
            names: BTreeSet::new(),
        }
    }

    /// The same set, with one more interface permitted.
    ///
    /// This is the seam the Go authoring lane needs: its components import `wasi:io/error`,
    /// `wasi:io/streams`, `wasi:cli/stdout` and `wasi:random/random` on top of the declared
    /// world, unconditionally, because TinyGo's runtime does. Permitting one is a decision
    /// about what the host will *bind*; an interface permitted here and left unbound still
    /// fails to instantiate, which is the safe direction.
    #[must_use]
    pub fn allowing(mut self, interface: impl Into<String>) -> Self {
        self.names.insert(interface.into());
        self
    }

    /// Whether an interface is in the set.
    #[must_use]
    pub fn contains(&self, interface: &str) -> bool {
        self.names.contains(interface)
    }

    /// Every permitted interface, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

impl Default for PermittedImports {
    fn default() -> Self {
        Self::declared_world()
    }
}

/// Every instance import a component declares, sorted.
///
/// Instances only — see [`PermittedImports`] for why the raw list is the wrong thing to
/// count.
#[must_use]
pub fn instance_imports(engine: &Engine, component: &Component) -> Vec<String> {
    let mut names: Vec<String> = component
        .component_type()
        .imports(engine)
        .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentInstance(_)))
        .map(|(name, _)| name.to_owned())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Refuse a component that imports anything outside the permitted set.
///
/// Called before instantiation, and before instantiation is even attempted, because the
/// point is to be independent of whether the host happens to link the offending interface.
///
/// `name` appears only in the message; it is whatever identifies the artifact to a reader —
/// a rule id, a path.
///
/// # Errors
///
/// Returns [`WasmError::ForbiddenImports`] naming every offending import and the set that was
/// permitted.
pub fn check_imports(
    engine: &Engine,
    name: &str,
    component: &Component,
    permitted: &PermittedImports,
) -> Result<(), WasmError> {
    let offenders: Vec<String> = instance_imports(engine, component)
        .into_iter()
        .filter(|import| !permitted.contains(import))
        .collect();

    if offenders.is_empty() {
        return Ok(());
    }

    Err(WasmError::ForbiddenImports {
        name: name.to_owned(),
        imports: offenders,
        permitted: permitted.names().map(str::to_owned).collect(),
    })
}

/// Where the compiled form of a rule came from.
///
/// Evidence rather than configuration. A caller cannot choose one; what it can do is read
/// afterwards which was reached, which is how "the slice is never the load path when a
/// mapped file is available" becomes something a test can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSource {
    /// Deserialized from a `.cwasm` on disk, which wasmtime maps.
    ///
    /// The path that has copy-on-write memory images. Roughly an eighth of the resident
    /// memory per instance and 18% faster to instantiate.
    Mapped,

    /// Compiled in this process from the component bytes.
    ///
    /// The deliberate fallback for a run with no writable cache directory. Correct, and it
    /// pays the compile cost on every run and forfeits the memory images.
    Embedded,
}

/// A component and where it was loaded from.
pub struct Loaded {
    /// The compiled component, ready to be resolved against the host world.
    pub component: Component,
    /// Which of the two paths produced it.
    pub source: LoadSource,
}

impl std::fmt::Debug for Loaded {
    /// Hand-written because `wasmtime::component::Component` implements no `Debug`, and
    /// `missing_debug_implementations` is a workspace warning.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Turns rule component bytes into compiled components, preferring a mapped file.
///
/// Shared rather than owned per worker: loading happens once per rule for the whole run, and
/// the counters below are only meaningful if every load goes through one of these. `&self`
/// throughout, so a run that loads its rules in parallel needs no lock.
#[derive(Debug)]
pub struct ComponentLoader {
    /// Where `.cwasm` files are written, or `None` for a run that has no writable cache.
    cache_dir: Option<PathBuf>,
    permitted: PermittedImports,
    mapped: AtomicUsize,
    embedded: AtomicUsize,
    compilations: AtomicUsize,
    /// Distinguishes the temporary files two threads compiling the same rule would otherwise
    /// both write to.
    sequence: AtomicUsize,
}

impl ComponentLoader {
    /// A loader that precompiles into `cache_dir`, or falls back to the slice when `None`.
    #[must_use]
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            permitted: PermittedImports::declared_world(),
            mapped: AtomicUsize::new(0),
            embedded: AtomicUsize::new(0),
            compilations: AtomicUsize::new(0),
            sequence: AtomicUsize::new(0),
        }
    }

    /// A loader writing into a project's own [`COMPONENT_CACHE_PATH`].
    #[must_use]
    pub fn for_project_root(root: &Path) -> Self {
        Self::new(Some(root.join(COMPONENT_CACHE_PATH)))
    }

    /// A loader with nowhere to write, which therefore always takes the fallback.
    ///
    /// What `--no-cache` and a read-only checkout get. Named rather than spelled
    /// `new(None)` at every call site, so the decision is visible in a diff.
    #[must_use]
    pub fn without_cache() -> Self {
        Self::new(None)
    }

    /// The same loader, admitting a different set of imports.
    #[must_use]
    pub fn permitting(mut self, permitted: PermittedImports) -> Self {
        self.permitted = permitted;
        self
    }

    /// What this loader admits.
    #[must_use]
    pub const fn permitted(&self) -> &PermittedImports {
        &self.permitted
    }

    /// How many components were deserialized from a mapped file.
    #[must_use]
    pub fn mapped_loads(&self) -> usize {
        self.mapped.load(Ordering::Relaxed)
    }

    /// How many components were taken from the embedded slice instead.
    ///
    /// **Zero is the assertion, not a statistic**, for any run with a writable cache
    /// directory. Condition 2 of the decision record is exactly "never load a rule from the
    /// embedded byte slice when a mapped file is available", and a counter is the only form
    /// of that claim a test can fail.
    #[must_use]
    pub fn embedded_loads(&self) -> usize {
        self.embedded.load(Ordering::Relaxed)
    }

    /// How many components this loader compiled rather than deserializing.
    ///
    /// One per rule on a cold cache and zero on a warm one. Separate from the two counters
    /// above because compiling and mapping are independent: a cold run with a writable cache
    /// directory compiles *and* ends up mapped.
    #[must_use]
    pub fn compilations(&self) -> usize {
        self.compilations.load(Ordering::Relaxed)
    }

    /// Where this loader would keep the precompiled form of some component bytes.
    ///
    /// `None` when it has no cache directory. Public so a test can assert on the artifact
    /// rather than on the counters alone.
    #[must_use]
    pub fn artifact_path(&self, name: &str, bytes: &[u8]) -> Option<PathBuf> {
        self.cache_dir
            .as_ref()
            .map(|dir| dir.join(artifact_name(name, bytes)))
    }

    /// Load a rule component, preferring a mapped precompiled artifact.
    ///
    /// Three steps, in this order, and the order is the condition:
    ///
    /// 1. **A `.cwasm` already on disk**, deserialized from the file so wasmtime maps it.
    ///    An artifact that fails to load — written by a different wasmtime, under different
    ///    memory tunables, or truncated — is discarded rather than trusted, and step 2 runs.
    /// 2. **Compile once, write, and map what was written.** The written artifact is what
    ///    this run uses, not the in-process compilation, so a cold run gets the memory
    ///    images too.
    /// 3. **The embedded slice**, when there is nowhere to write. Counted, because a run that
    ///    reaches it silently is the failure this whole module is arranged to prevent.
    ///
    /// The import check runs on whichever component results, in every case. A warm artifact
    /// was checked by the run that wrote it, and checking it again costs a type reflection
    /// and removes "the artifact on disk was vouched for by an earlier run" from the set of
    /// things anyone has to believe.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::ForbiddenImports`] when the component reaches outside the
    /// permitted set, or [`WasmError::Engine`] when the bytes are not a component this engine
    /// can compile. A cache directory that cannot be written is **not** an error: it is what
    /// [`LoadSource::Embedded`] is for.
    pub fn load(&self, engine: &WasmEngine, name: &str, bytes: &[u8]) -> Result<Loaded, WasmError> {
        if let Some(path) = self.artifact_path(name, bytes) {
            if path.is_file() {
                if let Some(component) = map(engine.engine(), &path) {
                    self.mapped.fetch_add(1, Ordering::Relaxed);
                    return self.admit(engine, name, component, LoadSource::Mapped);
                }
                // A stale or truncated artifact. Removing it is not required — the write
                // below replaces it — but leaving one that cannot be loaded means paying the
                // failed attempt on every subsequent run.
                drop(std::fs::remove_file(&path));
            }

            // Counted after the fact rather than before it, so the number means "components
            // this loader compiled" and not "components it tried to".
            let compiled = engine.compile(bytes)?;
            self.compilations.fetch_add(1, Ordering::Relaxed);
            if self.persist(&compiled, &path)
                && let Some(component) = map(engine.engine(), &path)
            {
                self.mapped.fetch_add(1, Ordering::Relaxed);
                return self.admit(engine, name, component, LoadSource::Mapped);
            }

            // The directory is not writable, or what was written could not be read back.
            // The compiled component is correct either way; what it lacks is the file behind
            // it, which is what makes this the fallback rather than a failure.
            self.embedded.fetch_add(1, Ordering::Relaxed);
            return self.admit(engine, name, compiled, LoadSource::Embedded);
        }

        let compiled = engine.compile(bytes)?;
        self.compilations.fetch_add(1, Ordering::Relaxed);
        self.embedded.fetch_add(1, Ordering::Relaxed);
        self.admit(engine, name, compiled, LoadSource::Embedded)
    }

    /// Check what a component reaches for, and hand it back if the answer is acceptable.
    fn admit(
        &self,
        engine: &WasmEngine,
        name: &str,
        component: Component,
        source: LoadSource,
    ) -> Result<Loaded, WasmError> {
        check_imports(engine.engine(), name, &component, &self.permitted)?;
        Ok(Loaded { component, source })
    }

    /// Write a compiled component's serialized form to `path`, atomically.
    ///
    /// Returns whether it is now there. Every failure is a `false` rather than an error:
    /// there is nothing a caller could usefully do about an unwritable cache directory except
    /// take the fallback, which is what the caller does.
    ///
    /// Written beside the target and renamed over it, for the reason `lanekeep_cache::Store`
    /// gives: a rename within a directory is atomic, so a concurrent reader sees either the
    /// whole previous artifact or the whole new one. That is load-bearing here in a way it is
    /// not for the cache, because the reader of this file hands it to
    /// `Component::deserialize_file`, whose contract is about what the bytes are.
    fn persist(&self, component: &Component, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
        let Ok(serialized) = component.serialize() else {
            return false;
        };

        // The process id alone is not enough: two threads in one process compiling the same
        // rule would derive the same temporary name from the same target.
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
        if std::fs::write(&temporary, &serialized).is_err() {
            drop(std::fs::remove_file(&temporary));
            return false;
        }
        if std::fs::rename(&temporary, path).is_err() {
            drop(std::fs::remove_file(&temporary));
            return false;
        }
        true
    }
}

/// Deserialize a precompiled artifact from a file, so wasmtime maps it.
///
/// `None` on any failure, and every failure here is recoverable by recompiling: an artifact
/// written by a different wasmtime, one compiled under different memory tunables — wasmtime
/// refuses those by name, saying "Module was compiled with a memory reservation of
/// '4294967296' but '1048576' is expected for the host" — a truncated file, a file that is
/// not an artifact at all.
///
/// # Safety
///
/// `Component::deserialize_file` is `unsafe` because a precompiled artifact is machine code
/// that wasmtime maps and jumps into, so a caller is asserting the file is one wasmtime
/// produced rather than something shaped like it. Three things carry that here.
///
/// Every artifact this reads was written by [`ComponentLoader::persist`] from
/// `Component::serialize`, by atomic rename of a fully written temporary — so a reader sees a
/// complete artifact or no file, never a partial one.
///
/// wasmtime validates its own header, its version, the target triple and twenty-six compiled
/// tunables before it maps anything, and returns an error rather than proceeding when any of
/// them disagree. That is what makes a stale artifact a recompile instead of undefined
/// behavior, and it is checked rather than assumed — `wasmtime-47.0.3`'s
/// `src/engine/serialization.rs` is where those comparisons are.
///
/// What is left is a file that something with write access to the project's `.lanekeep/`
/// directory replaced. That is the same authority as replacing the rule component itself, or
/// the configuration naming it, so it widens nothing — but it is the honest boundary and it
/// is stated rather than elided. A run that does not want it has `--no-cache`, which is
/// [`ComponentLoader::without_cache`] and never reads a file at all.
#[expect(
    unsafe_code,
    reason = "Component::deserialize_file is the mapped load path, and mapping is the whole \
              of what this module exists to do; the contract is discharged above"
)]
fn map(engine: &Engine, path: &Path) -> Option<Component> {
    // SAFETY: `path` is inside a cache directory this process owns, and every artifact there
    // was written by `ComponentLoader::persist` from `Component::serialize` on this same
    // engine configuration, by atomic rename. wasmtime re-validates the header, version,
    // target triple and compiled tunables and errors rather than mapping when they disagree,
    // which is what the `.ok()` below turns into a recompile.
    unsafe { Component::deserialize_file(engine, path) }.ok()
}

/// The filename a component's precompiled form is kept under.
///
/// Content-addressed, so a changed component is a different file rather than a stale one:
/// there is no invalidation step to get wrong, and two checkouts of different revisions
/// sharing a cache directory do not fight. The rule's name is a prefix purely so a human
/// looking at the directory can tell what is in it.
fn artifact_name(name: &str, bytes: &[u8]) -> String {
    let digest = blake3::hash(bytes).to_hex();
    let digest: String = digest.chars().take(DIGEST_CHARS).collect();
    format!("{}-{digest}.{ARTIFACT_EXTENSION}", sanitize(name))
}

/// A rule name reduced to something that is safe as one path component.
///
/// Rule ids are namespaced — `lanekeep/no-unwrap` — so they carry separators, and an id is
/// configuration rather than something this crate controls. Mapping everything outside a
/// conservative set to `_` means no id can decide where the artifact is written; the digest
/// is what makes the name unique, so collapsing two ids to the same prefix costs nothing.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .take(NAME_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "rule".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_permitted_set_is_the_declared_world_and_nothing_else() {
        let permitted = PermittedImports::default();
        assert!(permitted.contains(HOST_INTERFACE));
        assert!(!permitted.contains("wasi:clocks/wall-clock@0.2.3"));
        assert_eq!(permitted.names().collect::<Vec<_>>(), vec![HOST_INTERFACE]);
    }

    #[test]
    fn a_permitted_set_can_be_widened_without_losing_what_it_had() {
        // The shape sub-project 5 needs: five interfaces, named, with the declared world
        // still in the set.
        let permitted = PermittedImports::declared_world()
            .allowing("wasi:io/error@0.2.0")
            .allowing("wasi:io/streams@0.2.0")
            .allowing("wasi:cli/stdout@0.2.0")
            .allowing("wasi:random/random@0.2.0");
        assert_eq!(permitted.names().count(), 5);
        assert!(permitted.contains(HOST_INTERFACE));
        assert!(permitted.contains("wasi:random/random@0.2.0"));
        assert!(
            !permitted.contains("wasi:filesystem/types@0.2.0"),
            "widening admits what was named and not a family"
        );
    }

    #[test]
    fn an_empty_set_admits_nothing() {
        assert!(!PermittedImports::none().contains(HOST_INTERFACE));
    }

    #[test]
    fn a_rule_id_cannot_decide_where_its_artifact_is_written() {
        // Namespaced ids carry separators, and a hostile one carries more than that.
        let name = artifact_name("../../etc/passwd", b"bytes");
        assert!(
            !name.contains('/') && !name.contains(".."),
            "the name must be one path component: {name}"
        );
        assert_eq!(
            Path::new(&name).components().count(),
            1,
            "a filename with more than one component would escape the cache directory"
        );
    }

    #[test]
    fn the_artifact_name_changes_when_the_component_does() {
        let one = artifact_name("rule", b"first");
        let two = artifact_name("rule", b"second");
        assert_ne!(
            one, two,
            "content-addressing is the whole invalidation strategy"
        );
        assert_eq!(
            one,
            artifact_name("rule", b"first"),
            "and the same bytes have to give the same name, or nothing is ever reused"
        );
    }

    #[test]
    fn two_rules_with_the_same_bytes_still_get_their_own_artifacts() {
        assert_ne!(artifact_name("a", b"same"), artifact_name("b", b"same"));
    }

    #[test]
    fn a_name_that_sanitizes_away_entirely_still_produces_a_filename() {
        let name = artifact_name("///", b"bytes");
        assert!(name.starts_with("___-"), "{name}");
        assert!(!artifact_name("", b"bytes").starts_with('-'));
    }

    #[test]
    fn a_loader_without_a_cache_directory_has_nowhere_to_put_an_artifact() {
        assert!(
            ComponentLoader::without_cache()
                .artifact_path("rule", b"bytes")
                .is_none()
        );
    }

    #[test]
    fn a_loader_puts_its_artifacts_under_the_project_cache_directory() {
        let loader = ComponentLoader::for_project_root(Path::new("/projects/example"));
        let path = loader
            .artifact_path("rule", b"bytes")
            .expect("a loader with a cache directory has a path");
        assert!(
            path.starts_with(Path::new("/projects/example").join(COMPONENT_CACHE_PATH)),
            "{}",
            path.display()
        );
        assert_eq!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some(ARTIFACT_EXTENSION)
        );
    }
}
