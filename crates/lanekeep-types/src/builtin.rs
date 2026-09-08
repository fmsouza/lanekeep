//! The bounded provider: this crate's own oracle, plus the files it is allowed to open.
//!
//! Run-scoped state lives here rather than on [`TypeScriptOracle`](crate::TypeScriptOracle),
//! which owns exactly one parse and must stay that way. What this holds is a parser, a cache
//! of parsed declaration files and the memo of paths that were not there — so a library's
//! `.d.ts` is parsed once per run whatever imports it, and a miss is not re-probed per query.
//!
//! # Locks
//!
//! Every one is uncontended in the ordinary case and none is held across a call that can
//! block on anything but the filesystem. Poisoning is treated as "take the value anyway", the
//! posture `lanekeep_core::files::FileAccess` documents on its own memo: nothing under these
//! locks can panic, and refusing to answer because an unrelated worker died would turn a
//! rule's question into a failure with nothing to do with it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lanekeep_core::{FileAccess, FilePath};
use lanekeep_lang::Language;
use lanekeep_lang::binding::ImportedName;

use crate::declarations::{
    Declaration, ExportTarget, Exported, declared_name, find_export, target_node,
};
use crate::oracle::{ImportResolution, TypeScriptOracle, TypeScriptSupport};
use crate::provider::{Query, TypeProvider};
use crate::resolve::resolve_specifier;
use crate::types::{Symbol, Type};

/// How far a chain of re-exports is followed.
///
/// The same figure the oracle's own recursion bound uses, for the same reason: exceeding it
/// is indistinguishable from not knowing, which is already a first-class answer. Fixed rather
/// than measured — a bound that depended on elapsed time would put the clock in the cache key.
const MAX_EXPORT_DEPTH: u32 = 16;

/// The provider that reads declaration files with this crate's own oracle.
pub struct BuiltinProvider {
    support: TypeScriptSupport,
    /// One parser for the run's declaration files, behind a lock.
    ///
    /// Not a second parse of any file the engine already parsed: this opens `node_modules`
    /// and sibling declaration files, which are not in the corpus and have no shared tree.
    ///
    parser: Mutex<tree_sitter::Parser>,
    /// Declaration files parsed so far this run, by path.
    ///
    /// A `BTreeMap`, per the ordering invariant, and behind a lock because rayon runs one
    /// worker per file and they share this provider. A library's `.d.ts` is parsed once
    /// whatever imports it, which is the difference between a 500 KB `typescript.d.ts` costing
    /// tens of milliseconds once and costing them per importing file.
    declarations: Mutex<BTreeMap<FilePath, Arc<Declaration>>>,
    /// Paths that were not there.
    ///
    /// Separate from the map rather than an `Option` value in it, so the common lookup does
    /// not allocate an `Option<Arc<_>>` per hit. The read itself is already recorded by
    /// `FileAccess`; this only stops the provider re-asking within one run.
    misses: Mutex<BTreeSet<FilePath>>,
}

impl fmt::Debug for BuiltinProvider {
    /// Hand-written because neither `TypeScriptSupport` nor `tree_sitter::Parser` is
    /// `Debug`, the same reason and the same shape as the oracle's own impl.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinProvider").finish_non_exhaustive()
    }
}

impl BuiltinProvider {
    /// Confirm a grammar speaks TypeScript and build a provider over it.
    ///
    /// `None` on the same two conditions [`TypeScriptSupport::probe`] refuses on — a grammar
    /// without the vocabulary this oracle reads, or a language with no binding resolver —
    /// plus a third: a grammar the parser will not accept at all. Each would otherwise
    /// produce confident nonsense rather than an error.
    ///
    /// The only constructor. A provider must *parse* declaration files, so it cannot be
    /// built from a resolver alone.
    #[must_use]
    pub fn probe(language: &dyn Language) -> Option<Self> {
        let support = TypeScriptSupport::probe(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        Some(Self {
            support,
            parser: Mutex::new(parser),
            declarations: Mutex::new(BTreeMap::new()),
            misses: Mutex::new(BTreeSet::new()),
        })
    }

    /// The parser, whether or not another thread died holding it.
    fn parser(&self) -> MutexGuard<'_, tree_sitter::Parser> {
        self.parser.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An oracle over a question's own file, able to follow imports out of it.
    fn oracle_with<'q>(&'q self, q: &Query<'q>, imports: &'q Imports<'q>) -> TypeScriptOracle<'q> {
        TypeScriptOracle::new(&self.support, q.tree, q.source).with_imports(q.file, imports)
    }

    /// The parsed declaration file at `path`, read and parsed once per run.
    ///
    /// `None` when nothing is there, when it is not text, or when the grammar refuses it —
    /// three different reasons and one answer, because a rule can do nothing different with
    /// any of them and a rule that branched on the difference would give different answers on
    /// different machines.
    #[must_use]
    pub fn declaration(&self, files: &FileAccess, path: &FilePath) -> Option<Arc<Declaration>> {
        if self.misses().contains(path) {
            return None;
        }
        if let Some(found) = self.declarations().get(path) {
            return Some(Arc::clone(found));
        }

        let Ok(Some(source)) = files.read(path.as_str()) else {
            self.misses().insert(path.clone());
            return None;
        };
        let Some(parsed) = Declaration::parse(path.clone(), source, &mut self.parser()) else {
            self.misses().insert(path.clone());
            return None;
        };
        let parsed = Arc::new(parsed);
        self.declarations()
            .insert(path.clone(), Arc::clone(&parsed));
        Some(parsed)
    }

    fn declarations(&self) -> MutexGuard<'_, BTreeMap<FilePath, Arc<Declaration>>> {
        self.declarations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn misses(&self) -> MutexGuard<'_, BTreeSet<FilePath>> {
        self.misses.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follow `name` from `file` through re-exports to the file and name that declare it.
    ///
    /// `None` when a link cannot be read, when the name is nowhere, or when the chain
    /// exceeded `MAX_EXPORT_DEPTH` — one answer for the three, because a rule can do
    /// nothing different with any of them.
    #[must_use]
    pub fn export_target(
        &self,
        files: &FileAccess,
        file: &FilePath,
        name: &str,
    ) -> Option<ExportTarget> {
        let mut visited = BTreeSet::new();
        self.walk_export(files, file, name, 0, &mut visited)
    }

    fn walk_export(
        &self,
        files: &FileAccess,
        file: &FilePath,
        name: &str,
        depth: u32,
        visited: &mut BTreeSet<(FilePath, String)>,
    ) -> Option<ExportTarget> {
        if depth >= MAX_EXPORT_DEPTH {
            return None;
        }
        // The visited set rather than the bound alone. `export * from` in both directions is
        // a shape real packages ship, and a bound would turn an unbounded walk into a merely
        // slow one — sixteen files opened and parsed per query, on a corpus, is not a cost
        // worth paying to reach the same `None`.
        if !visited.insert((file.clone(), name.to_owned())) {
            return None;
        }

        let decl = self.declaration(files, file)?;
        match find_export(&decl, name)? {
            Exported::Here(node) => Some(ExportTarget {
                file: file.clone(),
                name: declared_name(&decl, node).unwrap_or_else(|| name.to_owned()),
            }),
            Exported::From {
                specifier,
                name: exported,
            } => {
                let next = resolve_specifier(files, file, &specifier)?;
                self.walk_export(files, &next, &exported, depth.saturating_add(1), visited)
            }
            // A module object has no single declaration, so there is nothing to walk to.
            Exported::Namespace { .. } => None,
            // Source order, first hit wins: `find_map` short-circuits, so a corpus does not
            // pay for every star source once one of them answers.
            Exported::Star(sources) => sources.iter().find_map(|specifier| {
                let next = resolve_specifier(files, file, specifier)?;
                self.walk_export(files, &next, name, depth.saturating_add(1), visited)
            }),
        }
    }

    /// The declaring file and node for an imported name, or nothing readable.
    ///
    /// One place, because all four hook methods start here and the difference between them is
    /// only what they do with the node.
    fn imported(
        &self,
        files: &FileAccess,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
    ) -> Option<(Arc<Declaration>, ExportTarget)> {
        // A namespace import binds the whole module object, which has no declaration to walk
        // to — the same `None` `Exported::Namespace` produces one layer down.
        let wanted = match name {
            ImportedName::Named(exported) => exported.clone(),
            ImportedName::Default => "default".to_owned(),
            ImportedName::Namespace => return None,
        };
        let entry = resolve_specifier(files, from, module)?;
        let target = self.export_target(files, &entry, &wanted)?;
        let decl = self.declaration(files, &target.file)?;
        Some((decl, target))
    }
}

/// One call's worth of a provider, so the oracle can ask it questions.
///
/// `ImportResolution`'s methods take no [`FileAccess`], because an oracle has no business
/// knowing there is one — but a provider needs the caller's, and the caller's changes per
/// question. Pairing the two in a value that lives exactly as long as the call is what lets
/// the trait stay narrow.
struct Imports<'a> {
    provider: &'a BuiltinProvider,
    files: &'a FileAccess,
}

impl ImportResolution for Imports<'_> {
    fn imported_value_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Option<Type> {
        let (decl, target) = self.provider.imported(self.files, from, module, name)?;
        let node = target_node(&decl, &target.name)?;
        // Typed in the *declaring* file's own context, with the same resolver and the same
        // resolution, so a chain of re-exports and aliases is one recursion under one bound.
        let nested = Imports {
            provider: self.provider,
            files: self.files,
        };
        let oracle = TypeScriptOracle::new(&self.provider.support, &decl.tree, &decl.source)
            .with_imports(&decl.path, &nested);
        oracle.declaration_type_from(node, depth)
    }

    fn imported_alias_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Option<Type> {
        let (decl, target) = self.provider.imported(self.files, from, module, name)?;
        let node = target_node(&decl, &target.name)?;
        // Only an alias. A class or an interface keeps its use-site symbol, which is what the
        // caller does when this answers `None` — see `ImportResolution`'s own documentation.
        if node.kind() != "type_alias_declaration" {
            return None;
        }
        let value = node.child_by_field_name("value")?;
        let nested = Imports {
            provider: self.provider,
            files: self.files,
        };
        let oracle = TypeScriptOracle::new(&self.provider.support, &decl.tree, &decl.source)
            .with_imports(&decl.path, &nested);
        oracle.type_of_from(value, depth)
    }

    /// Nothing yet — Task 12 fills this in, together with `return_type_of` itself.
    fn imported_return_type(
        &self,
        _from: &FilePath,
        _module: &str,
        _name: &ImportedName,
        _depth: u32,
    ) -> Option<Type> {
        None
    }

    fn imported_export(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
    ) -> Option<ExportTarget> {
        self.provider
            .imported(self.files, from, module, name)
            .map(|(_, target)| target)
    }
}

impl TypeProvider for BuiltinProvider {
    fn type_of(&self, q: Query<'_>) -> Option<Type> {
        let imports = Imports {
            provider: self,
            files: q.files,
        };
        self.oracle_with(&q, &imports).type_of(q.node)
    }

    fn symbol_of(&self, q: Query<'_>) -> Option<Symbol> {
        let imports = Imports {
            provider: self,
            files: q.files,
        };
        self.oracle_with(&q, &imports).symbol_of(q.node)
    }

    /// Nothing yet — Task 12 fills this in.
    ///
    /// `None` rather than a guess for the same reason every other arm answers `None`: an
    /// unimplemented arm and an unanswerable question are the same thing to a rule, and both
    /// are silence rather than a wrong report.
    fn return_type_of(&self, _q: Query<'_>) -> Option<Type> {
        None
    }

    /// Nothing yet — Task 13 fills this in. See [`Self::return_type_of`].
    fn is_assignable_to(&self, _q: Query<'_>, _module: &str, _name: &str) -> Option<bool> {
        None
    }

    /// `type_of` and `symbol_of` now follow an import into the declaring file, so an
    /// unresolvable import does make an answer partial; this still answers `true` because the
    /// file-level walk that decides it is Task 14's, and until then a partial answer is
    /// indistinguishable here.
    fn complete(&self, _q: Query<'_>) -> bool {
        true
    }

    fn identity(&self) -> Vec<u8> {
        // Tagged as well as hashed. `oracle_identity` alone would let a future provider that
        // happened to derive its identity the same way collide with this one, and the tag is
        // what makes "which provider answered" part of the key rather than an inference.
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(b"builtin:");
        out.extend_from_slice(&crate::oracle_identity());
        out
    }
}

/// `BuiltinProvider` is shareable, checked at compile time rather than believed.
///
/// The engine holds one in an `Arc` that rayon moves between workers, so a field that is not
/// `Send + Sync` must stop the build here rather than as an unsatisfied bound two crates away
/// — the reasoning `FileAccess`'s own `assert_shareable` block gives.
const _: () = {
    const fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<BuiltinProvider>();
};
