//! The bounded provider: this crate's own oracle, plus the files it is allowed to open.
//!
//! Run-scoped state lives here rather than on [`TypeScriptOracle`](crate::TypeScriptOracle),
//! which owns exactly one parse and must stay that way. What this holds is a parser and a
//! cache of parsed declaration files, keyed by content hash — so a library's `.d.ts` is
//! parsed once per version of its bytes, not once per run: a provider a session holds across
//! requests (#191) keeps every entry whose file has not moved, and
//! [`TypeProvider::begin_run`] no longer throws that cache away. [`TypeProvider::revalidate`]
//! is what drops a hash-mismatched entry proactively, ahead of a query finding out the hard
//! way.
//!
//! # Locks
//!
//! Poisoning is treated as "take the value anyway", the posture
//! `lanekeep_core::files::FileAccess` documents on its own memo: nothing under these locks can
//! panic, and refusing to answer because an unrelated worker died would turn a rule's question
//! into a failure with nothing to do with it.
//!
//! The parser's lock is the one that is genuinely *held across work* — a whole parse, which
//! for a large `typescript.d.ts` is tens of milliseconds — and it is a plain mutex rather than
//! an entry API. So two workers that reach an uncached declaration file at the same moment
//! both parse it and the second write wins. That is benign: the parses are of the same bytes
//! and produce the same answers, the cost is at most one extra parse per worker that races,
//! and the alternative is holding a lock across the filesystem read as well.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lanekeep_core::{AnalysisBudget, FileAccess, FilePath};
use lanekeep_lang::Language;
use lanekeep_lang::binding::ImportedName;

use crate::declarations::{
    Declaration, ExportTarget, Exported, declared_in, declared_name, find_export,
    imports_with_names, target_node,
};
use crate::oracle::{Followed, ImportResolution, MAX_DEPTH, TypeScriptOracle, TypeScriptSupport};
use crate::provider::{BeginRunError, Query, TypeProvider};
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
    /// The main grammar's shape digest — [`lanekeep_lang::grammar_digest`], its node kinds
    /// and fields — held from probe time beside the resolver's own analysis identity. Both
    /// are what [`TypeProvider::identity`] folds, with the tsx grammar's digest behind a
    /// presence byte, so *which* grammar parses `.ts` and which parses `.tsx` are both in the
    /// key. `TypeScript` and `Tsx` share one analysis identity, and a fold over that alone
    /// let a provider over the TSX grammar warm the cache of one over TypeScript.
    grammar_digest: [u8; 32],
    /// The resolver's analysis identity, from the language that was probed.
    analysis_identity: [u8; 32],
    /// One parser per grammar this provider opens, behind a lock — this one for every path
    /// that is not `.tsx`, the second grammar's (when one was given at probe time) for the
    /// rest, chosen by the resolved path's extension in [`Self::parser_for`].
    ///
    /// **It does parse corpus files a second time**, and an earlier version of this comment
    /// claimed the opposite. `RELATIVE_SUFFIXES` prefers `.ts` over `.d.ts`, so a relative
    /// import of a project source — `import { parsed } from '../lib/ids'` — resolves to the
    /// very file the engine parses itself, and this parses it again into its own arena. Once
    /// per run per file, not once per importer, so the cost is bounded by the number of
    /// distinct files reached through imports rather than by the number of imports.
    ///
    /// Sharing the engine's node arena would remove it and is a separate seam: the arena is
    /// keyed by the run's file list, and a declaration file under `node_modules` is not in it
    /// at all, so the two would have to meet somewhere neither owns today.
    ///
    /// This file is on `local/one-parser-per-file`'s `allow` list in `lanekeep.json` for
    /// exactly that reason, and this paragraph is the rationale the list cannot carry — JSON
    /// has no comments. The rule is right about what it sees; the second parser is deliberate,
    /// and the entry is what says a reviewer has already weighed it.
    parser: Mutex<tree_sitter::Parser>,
    /// The second grammar's parser, behind its **own** lock — never this one's — together
    /// with the identity of the language probed to build it.
    ///
    /// `None` when no second grammar was given. The resolver still reaches a `.tsx` sibling
    /// then, but the main grammar reads its JSX as `ERROR` nodes, and `complete()` counts
    /// those as unread: an honest "incomplete" rather than a confidently wrong answer.
    tsx: Option<TsxParser>,
    /// Declaration files parsed so far, by path — kept across `begin_run`, not cleared by it.
    ///
    /// A `BTreeMap`, per the ordering invariant, and behind a lock because rayon runs one
    /// worker per file and they share this provider. A library's `.d.ts` is parsed once per
    /// version of its bytes, which is the difference between a 500 KB `typescript.d.ts`
    /// costing tens of milliseconds once and costing them per importing file — and, for a
    /// provider a session holds across requests (#191), the difference between costing them
    /// once per session and once per request.
    ///
    /// Entries carry the hash their bytes had, and [`Self::declaration`] compares it against
    /// what the *asking* access read — see that method for why serving by path alone writes an
    /// entry describing neither version of a file rewritten mid-run. That same hash check is
    /// what makes it safe for [`TypeProvider::begin_run`] to leave this memo alone: a stale
    /// entry is never served, so nothing here needs a cold start. [`TypeProvider::revalidate`]
    /// drops a mismatched entry ahead of time, so a held provider is not carrying a parse
    /// tree for a version of a file it will never answer about again.
    declarations: Mutex<BTreeMap<FilePath, Arc<Declaration>>>,
    /// Whether each file's imports all resolved, decided once per file.
    ///
    /// The pass behind it is eager — every import is resolved, not only the ones a rule asked
    /// about — which is what records an absent declaration as a dependency even when nothing
    /// went looking for the type behind it.
    ///
    /// **Also keyed by path alone, and this is the memo where that bites hardest.**
    /// [`Self::complete`] answers from it before any `resolve_specifier` runs, so a second
    /// request served from a stale entry records *no import dependencies at all* — a cache
    /// entry with nothing in it to invalidate. A provider held across runs must clear this
    /// one rather than drop by hash: a `bool` has no hash to drop by.
    completeness: Mutex<BTreeMap<FilePath, bool>>,
    /// How many times [`Self::declaration`] has actually parsed a file, this process.
    ///
    /// Test-only: the seam that lets a pin distinguish "answered from the memo" from
    /// "parsed again" without inferring it from timing, which would flake on a loaded
    /// machine. Nothing outside `#[cfg(test)]` reads it, so it costs nothing in a real run.
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
}

impl fmt::Debug for BuiltinProvider {
    /// Hand-written because neither `TypeScriptSupport` nor `tree_sitter::Parser` is
    /// `Debug`, the same reason and the same shape as the oracle's own impl.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinProvider").finish_non_exhaustive()
    }
}

/// The second parser: the grammar for the `.tsx` files the resolver reaches, behind its own
/// lock so a `.ts` parse and a `.tsx` parse never wait on each other.
struct TsxParser {
    parser: Mutex<tree_sitter::Parser>,
    /// The probed grammar's shape digest, folded into [`TypeProvider::identity`] so the
    /// grammar a `.tsx` answer was read with is part of the cache key that answer lands
    /// under — the grammar's own, not the analysis identity every language in the family
    /// shares.
    grammar_digest: [u8; 32],
}

impl TsxParser {
    /// A parser over the given grammar, or `None` when the grammar will not load — the same
    /// refusal the main probe makes, for the same reason: a parser that cannot be built is
    /// a provider that cannot read what the resolver hands it.
    fn probe(language: &dyn Language) -> Option<Self> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        Some(Self {
            parser: Mutex::new(parser),
            grammar_digest: lanekeep_lang::grammar_digest(&language.grammar()),
        })
    }
}

/// Whether a project-relative path names a `.tsx` file.
///
/// Case-insensitive because the filesystem decides case, and the parse has to agree with the
/// resolver's suffix probe — and with `LanguageRegistry::for_path`, which lowercases too — on
/// whatever case the tree spells. The stem check keeps a hidden `.tsx` — no stem at all,
/// at the root or in any directory — from counting as one.
fn extension_is_tsx(path: &str) -> bool {
    match path.rsplit_once('.') {
        Some((stem, extension)) => {
            !stem.is_empty() && !stem.ends_with('/') && extension.eq_ignore_ascii_case("tsx")
        }
        None => false,
    }
}

/// Why an export walk did not end at a declaration.
///
/// `complete()` tells the two apart and nothing else does: [`BuiltinProvider::export_target`]
/// folds both to `None`, because a rule can do nothing different with either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unreached {
    /// A link could not be read: a specifier that resolves to nothing, a file that will not
    /// parse, a declaration the parser did not finish, or a chain past `MAX_EXPORT_DEPTH`.
    Unread,
    /// Every link was read and none declares the name in a shape this walk models — a
    /// namespace binding, or a module whose members reach the importer some way the walk
    /// does not follow, `export = X` beside `declare namespace X` above all.
    Unmodeled,
}

/// One [`BuiltinProvider::is_assignable_to`] call's bookkeeping.
///
/// Three fields rather than three parameters, so [`BuiltinProvider::assignable`] and
/// [`BuiltinProvider::heritage_assignable`] keep the argument count `clippy::too_many_arguments`
/// allows — the same reason their `at` triple is bundled.
struct Walk {
    /// Declarations on the *current path*, each with its position on that path.
    ///
    /// Path-scoped rather than seen-once: a sibling branch that reaches the same ancestor
    /// through a different path must be answered rather than told it was already walked. Only
    /// a cycle on the current path is meant to be cut.
    ///
    /// The index is what makes [`Walk::lowlink`] work: a cycle is a back-edge to a position on
    /// the current path, and how far back it reaches is what decides which declarations above
    /// it may still memoize.
    visiting: BTreeMap<(FilePath, String), usize>,
    /// What each declaration answered, for the duration of this call.
    ///
    /// The path-scoped set above cuts cycles and does nothing about *re-convergence*: a graph
    /// where every declaration extends `b` parents that later meet again has `b^depth` paths
    /// through a linear number of declarations — 4^12 ≈ 16.8 million through forty-eight, which
    /// is minutes inside a single uninterruptible host call. Keyed on the resolved
    /// `(declaring file, declared name)` pair, which is the only identity that survives
    /// crossing a file.
    ///
    /// Not keyed on depth. **Nothing exhausted by the depth bound is written here at all** —
    /// see [`Walk::exhausted`] — because an entry written from a truncated subtree is a `None`
    /// that would be read back at a shallower position where the walk would have answered. The
    /// walk order is a function of the input, so two runs still answer identically.
    answers: BTreeMap<(FilePath, String), Option<bool>>,
    /// How far back the current subtree has reached, as a position on the current path.
    ///
    /// [`usize::MAX`] for "nowhere", which is what makes `min` the whole update rule. A cycle
    /// cut on a key held at index `i` lowers this to `i`, and each frame folds its own value
    /// into its parent's on the way out — Tarjan's lowlink, for exactly Tarjan's reason: it is
    /// the cheapest thing that says *which* declarations an answer depended on the path for.
    ///
    /// A single global counter of cuts was the first spelling and was far too coarse. It said
    /// only "a cycle was cut somewhere under here", so one mutual pair at the bottom of a graph
    /// disabled the memo for every declaration above it, and a re-converging graph went back to
    /// `width^depth` paths — the exact cost the memo exists to remove.
    lowlink: usize,
    /// How many times the depth bound has truncated a subtree on this call.
    ///
    /// A subtree the bound cut answered about a *prefix* of the graph rather than about the
    /// declaration, so its `None` is a property of where it was reached from. Memoizing it
    /// hands that `None` to a later, shallower reach of the same declaration that the walk
    /// would have answered — which is what happened to a declaration named both far down a
    /// chain and directly by the root. Compared before and after a subtree, exactly as the
    /// lowlink is.
    exhausted: u32,
}

impl BuiltinProvider {
    /// Confirm a grammar speaks TypeScript and build a provider over it, with no second
    /// grammar — [`Self::probe_with`] is where one is added.
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
        Self::probe_with(language, None)
    }

    /// [`Self::probe`] with a second grammar, for the `.tsx` files the resolver reaches.
    ///
    /// The oracle's vocabulary is still confirmed against the *main* language alone, and the
    /// support built from it is what answers every question: the tsx grammar speaks the same
    /// node vocabulary — it is the same resolver, one grammar wider — so a `.tsx` sibling
    /// needs no second oracle, only a second parse.
    ///
    /// `None` when either grammar will not load into a parser, the main probe's own refusal
    /// unchanged: a resolver that reaches a `.tsx` file a provided grammar cannot parse is
    /// one that would answer from `ERROR` nodes.
    #[must_use]
    pub fn probe_with(language: &dyn Language, tsx: Option<&dyn Language>) -> Option<Self> {
        let support = TypeScriptSupport::probe(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        let tsx = match tsx {
            None => None,
            Some(tsx) => Some(TsxParser::probe(tsx)?),
        };
        Some(Self {
            support,
            grammar_digest: lanekeep_lang::grammar_digest(&language.grammar()),
            analysis_identity: language.analysis_identity(),
            parser: Mutex::new(parser),
            tsx,
            declarations: Mutex::new(BTreeMap::new()),
            completeness: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Whether this provider has a grammar for the dialect `path` is written in.
    ///
    /// `.tsx` needs the second grammar; everything else the resolver reaches is read by the
    /// main one. A path that answers `false` is not read at all — see `walk_export` and
    /// `complete()` — because a parse in the wrong dialect is wrong even when it is clean:
    /// `<Foo>bar` is a type assertion to the TypeScript grammar and JSX to the TSX one.
    fn reads_dialect_of(&self, path: &str) -> bool {
        self.tsx.is_some() || !extension_is_tsx(path)
    }

    /// The parser, whether or not another thread died holding it.
    fn parser(&self) -> MutexGuard<'_, tree_sitter::Parser> {
        self.parser.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The parser for the file at `path`: the second grammar's when the path's extension is
    /// `.tsx` — case-insensitively, so the parse agrees with the resolver's suffix probe on
    /// whatever case the tree spells — and a tsx grammar was given, the main one otherwise.
    ///
    /// One guard, whichever mutex it came out of: the caller cannot tell and need not, and
    /// the two locks are what keep a `.ts` parse and a `.tsx` parse from waiting on each
    /// other. No tsx grammar, every path answers from the main parser — including a `.tsx`
    /// one, whose JSX then becomes the `ERROR` nodes `complete()` counts.
    fn parser_for(&self, path: &str) -> MutexGuard<'_, tree_sitter::Parser> {
        let tsx = self.tsx.as_ref().filter(|_| extension_is_tsx(path));
        match tsx {
            Some(tsx) => tsx.parser.lock().unwrap_or_else(PoisonError::into_inner),
            None => self.parser(),
        }
    }

    /// An oracle over a question's own file, able to follow imports out of it.
    fn oracle_with<'q>(&'q self, q: &Query<'q>, imports: &'q Imports<'q>) -> TypeScriptOracle<'q> {
        TypeScriptOracle::new(&self.support, q.tree, q.source).with_imports(q.file, imports)
    }

    /// The parsed declaration file at `path`, parsed once per version of its bytes — kept
    /// across `begin_run` now, not cleared to force a re-parse per run.
    ///
    /// `None` when nothing is there, when it is not text, or when the grammar refuses it —
    /// three different reasons and one answer, because a rule can do nothing different with
    /// any of them and a rule that branched on the difference would give different answers on
    /// different machines.
    ///
    /// **One hash lookup per call, and bytes only when the parse is stale.** The cache is
    /// keyed by path, and two `FileAccess`es over one path can see two different files — a
    /// rewrite mid-run, which is routine under `--watch`. Served by path alone, the *second*
    /// importer would get the *first* version's parse while its own access recorded the new
    /// bytes' hash, and the entry written then describes neither version: a wrong answer that
    /// validates forever. So the hash decides, and [`FileAccess::hash_of`] answers it from the
    /// access's own memo without materializing the text — which is what the previous spelling
    /// paid, cloning a whole declaration file per importer and re-hashing it to compare
    /// against a digest the parse already carried.
    ///
    /// **Nothing memoizes the failures**, and nothing needs to. A path with no hash is
    /// re-probed on the next call, which costs one [`FileAccess::hash_of`] — and that access
    /// has a memo of its own, so within a run the second probe reads nothing from the disk.
    /// A memo here could only be keyed by path, having no hash to key on, so it would have to
    /// be cleared wholesale at the start of every run to keep a `.d.ts` installed between two
    /// runs from staying missing forever.
    ///
    /// A path that has *stopped* answering — deleted, or no longer text — has its parse
    /// dropped rather than left behind. The entry is unservable from here on, because every
    /// path through this method compares a hash first, so keeping it holds a whole declaration
    /// file's tree and source until the next `begin_run` for nothing.
    #[must_use]
    pub fn declaration(&self, files: &FileAccess, path: &FilePath) -> Option<Arc<Declaration>> {
        let Ok(Some(hash)) = files.hash_of(path.as_str()) else {
            self.declarations().remove(path);
            return None;
        };
        if let Some(found) = self.declarations().get(path)
            && found.hash == hash
        {
            return Some(Arc::clone(found));
        }

        let Ok(Some(source)) = files.read(path.as_str()) else {
            return None;
        };
        #[cfg(test)]
        self.parses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let parsed = Declaration::parse(
            path.clone(),
            source,
            &mut self.parser_for(path.as_str()),
            Arc::clone(self.support.resolver()),
        )?;
        let parsed = Arc::new(parsed);
        // Replaces rather than keeps: the bytes this access read are the ones the run is
        // answering about from here on.
        self.declarations()
            .insert(path.clone(), Arc::clone(&parsed));
        Some(parsed)
    }

    fn declarations(&self) -> MutexGuard<'_, BTreeMap<FilePath, Arc<Declaration>>> {
        self.declarations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn completeness(&self) -> MutexGuard<'_, BTreeMap<FilePath, bool>> {
        self.completeness
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// How many times [`Self::declaration`] has parsed a file, so far.
    #[cfg(test)]
    fn parses(&self) -> usize {
        self.parses.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Follow `name` from `file` through re-exports to the file and name that declare it.
    ///
    /// `None` when a link cannot be read, when the name is nowhere, or when the chain
    /// exceeded `MAX_EXPORT_DEPTH` — one answer for the three, because a rule can do
    /// nothing different with any of them. `complete()` can, and asks `export_walk` instead.
    #[must_use]
    pub fn export_target(
        &self,
        files: &FileAccess,
        file: &FilePath,
        name: &str,
    ) -> Option<ExportTarget> {
        self.export_walk(files, file, name).ok()
    }

    /// [`Self::export_target`], keeping why a walk did not end at a declaration.
    fn export_walk(
        &self,
        files: &FileAccess,
        file: &FilePath,
        name: &str,
    ) -> Result<ExportTarget, Unreached> {
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
    ) -> Result<ExportTarget, Unreached> {
        if depth >= MAX_EXPORT_DEPTH {
            return Err(Unreached::Unread);
        }
        // The visited set rather than the bound alone. `export * from` in both directions is
        // a shape real packages ship, and a bound would turn an unbounded walk into a merely
        // slow one — sixteen files opened and parsed per query, on a corpus, is not a cost
        // worth paying to reach the same answer. A cycle back to a pair already on the walk
        // declares nothing new, so it is a miss rather than an unread link.
        if !visited.insert((file.clone(), name.to_owned())) {
            return Err(Unreached::Unmodeled);
        }

        // A file in a dialect this provider has no grammar for is not read: parsed with the
        // main grammar, a JSX statement becomes an `ERROR` that covers only itself and the
        // clean statement beside it reads as read, and a parse that happens to be clean is
        // in the wrong dialect all the same. This is the refusal the old `RELATIVE_SUFFIXES`
        // omission made, kept where `resolve.rs` promises it. Before the parse, so the
        // answer records no read of a file it does not depend on.
        if !self.reads_dialect_of(file.as_str()) {
            return Err(Unreached::Unread);
        }
        let decl = self.declaration(files, file).ok_or(Unreached::Unread)?;
        let Some(exported) = find_export(&decl, name) else {
            // Nothing here exports the name. In a file the parser read whole that is a fact
            // about the module; in one it did not, the declaration may sit inside the span
            // the parser gave up on, and the honest answer is that it was not read.
            return Err(if decl.has_error {
                Unreached::Unread
            } else {
                Unreached::Unmodeled
            });
        };
        match exported {
            Exported::Here(node) => {
                // The one gate on a damaged declaration, for every caller of the walk:
                // `has_error()` on the reached node counts a `MISSING` token as well as an
                // `ERROR`, and it is a property of the node whichever tree it sits in.
                if node.has_error() {
                    return Err(Unreached::Unread);
                }
                Ok(ExportTarget {
                    file: file.clone(),
                    name: declared_name(&decl, node).unwrap_or_else(|| name.to_owned()),
                })
            }
            Exported::From {
                specifier,
                name: exported,
            } => {
                let next = resolve_specifier(files, file, &specifier).ok_or(Unreached::Unread)?;
                self.walk_export(files, &next, &exported, depth.saturating_add(1), visited)
            }
            // A module object has no single declaration, so there is nothing to walk to.
            Exported::Namespace { .. } => Err(Unreached::Unmodeled),
            // Source order, first hit wins, so a corpus does not pay for every star source
            // once one of them answers. A source that cannot be read is read past, the way
            // the walk always has — `export * from './generated'` beside a live source must
            // not silence every name the barrel re-exports — and it decides the verdict only
            // when no source answered: then the name may well sit in the file that could not
            // be read, and that is unread rather than absent.
            Exported::Star(sources) => {
                let mut unread = false;
                for specifier in &sources {
                    let Some(next) = resolve_specifier(files, file, specifier) else {
                        unread = true;
                        continue;
                    };
                    match self.walk_export(files, &next, name, depth.saturating_add(1), visited) {
                        Err(Unreached::Unmodeled) => {}
                        Err(Unreached::Unread) => unread = true,
                        found @ Ok(_) => return found,
                    }
                }
                Err(if unread {
                    Unreached::Unread
                } else {
                    Unreached::Unmodeled
                })
            }
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

    /// Whether `ty`, read in the file `(tree, source)` is the parse of, is the type declared
    /// at `target`.
    ///
    /// Nominal, never structural. `Some(false)` is a real answer — the walk completed and
    /// reached nothing — and `None` is "a link in the chain could not be read", which a rule
    /// must not treat as a negative: a project whose `node_modules` is absent would otherwise
    /// have every governed value reported. A parent declaration the parser only partly read
    /// is unreadable the same way — the walk refuses rather than answer over a damaged
    /// node — and that is read off the node itself (`has_error()`), so it holds in the
    /// asking file's tree exactly as in a declaration file's.
    ///
    /// `target` is resolved once, by [`Self::is_assignable_to`], rather than compared as a
    /// `(module, name)` pair at every step. A symbol's own `module` field cannot stand in for
    /// that comparison past the first hop: `TypeScriptOracle::symbol_at` reports a `Symbol`
    /// whose `module` field is `None` for a name declared *locally* in whichever file the
    /// oracle is currently reading, and every file this walk steps into is, from its own
    /// point of view, local — so a `Decimal` found by walking into `money`'s own declaration
    /// file carries no `module` at all, and a comparison against the string `"money"` would
    /// silently miss it. Comparing the *resolved* declaring file and name is the check that
    /// still holds after crossing files.
    fn assignable(
        &self,
        files: &FileAccess,
        // The file this walk currently stands in: its path, its tree, and its source, bundled
        // so the whole trio moves as one argument — `assignable`/`heritage_assignable` would
        // otherwise carry eight parameters apiece and trip `clippy::too_many_arguments`.
        at: (&FilePath, &tree_sitter::Tree, &str),
        ty: &Type,
        target: (&FilePath, &str),
        depth: u32,
        walk: &mut Walk,
    ) -> Option<bool> {
        let (at_path, tree, source) = at;
        if depth >= MAX_EXPORT_DEPTH {
            // Counted, so nothing computed above this point is memoized: the answer this
            // truncation produces is about the path, not about the declaration.
            walk.exhausted = walk.exhausted.saturating_add(1);
            return None;
        }
        match ty {
            Type::Primitive(_) => Some(false),
            Type::Union(members) => {
                // Every member, and `None` from any of them sinks the answer: a union one
                // member of which could not be read is not evidence about the union.
                let mut all = true;
                for member in members {
                    if !self.assignable(files, at, member, target, depth.saturating_add(1), walk)? {
                        all = false;
                    }
                }
                Some(all)
            }
            Type::Nominal {
                name: written,
                symbol,
            } => {
                // The resolver's own opinion, when it has one. Genuinely no answer — an
                // ambient global (`Date`, never declared or imported anywhere) — which no
                // resolver arm can bind because nothing binds it.
                //
                // `declared_in` is the fallback that tells an ambient global apart from a
                // name that only *looks* unbound — a spelling the resolver's walk does not
                // cover, or a construct a future grammar revision moves — over the *current*
                // file. When it also finds nothing, this is genuinely unreadable, matching
                // `symbol_at`'s own contract, which already returns `None` outright rather
                // than a `Symbol` with empty fields.
                let (declaring, declared) = if let Some(symbol) = symbol {
                    match &symbol.module {
                        Some(specifier) => {
                            let entry = resolve_specifier(files, at_path, specifier)?;
                            let exported = symbol.exported.as_deref()?;
                            let found = self.export_target(files, &entry, exported)?;
                            (found.file, found.name)
                        }
                        None => (at_path.clone(), written.clone()),
                    }
                } else {
                    declared_in(self.support.resolver().as_ref(), tree, source, written)?;
                    (at_path.clone(), written.clone())
                };

                if declaring == *target.0 && declared == target.1 {
                    return Some(true);
                }
                let key = (declaring.clone(), declared.clone());
                // Answered already, on some other path through this graph — see `Walk::answers`
                // for why one declaration's answer is the same wherever it recurs.
                if let Some(known) = walk.answers.get(&key) {
                    return *known;
                }
                if let Some(&reached) = walk.visiting.get(&key) {
                    // Already walked *on this path*. `false` rather than `None`: a cycle is a
                    // fully read program that does not reach the named type, not an
                    // unreadable one. Recorded as a back-edge to the position the cycle
                    // reaches, which is what stops the poison at the declarations really
                    // inside it rather than spreading it to the whole ancestor chain.
                    walk.lowlink = walk.lowlink.min(reached);
                    return Some(false);
                }

                // The position this declaration takes on the current path. Positions are
                // handed out by path length, so they increase strictly downward and a
                // back-edge to a smaller one is a cycle escaping this subtree.
                let index = walk.visiting.len();
                walk.visiting.insert(key.clone(), index);
                let outer_lowlink = walk.lowlink;
                walk.lowlink = usize::MAX;
                let exhausted_before = walk.exhausted;
                let result = if &declaring == at_path {
                    self.heritage_assignable(files, at, &declared, target, depth, walk)
                } else {
                    let decl = self.declaration(files, &declaring);
                    match decl {
                        Some(decl) => self.heritage_assignable(
                            files,
                            (&decl.path, &decl.tree, &decl.source),
                            &declared,
                            target,
                            depth,
                            walk,
                        ),
                        None => None,
                    }
                };
                // Removed once this node's own answer is known, so a sibling branch that
                // reaches the same ancestor through a different path is not told it was
                // "already walked" by a walk that has since returned.
                walk.visiting.remove(&key);
                let reached = walk.lowlink;
                // The parent inherits it: a back-edge past *this* node is one past every node
                // above it too. A back-edge that stopped here is `>= index`, which is larger
                // than any ancestor's own index and so cannot block one.
                walk.lowlink = outer_lowlink.min(reached);
                if reached >= index && walk.exhausted == exhausted_before {
                    // Nothing under it reached back past it and nothing under it was truncated
                    // by the depth bound, so this answer is a property of the declaration
                    // rather than of the path that reached it.
                    walk.answers.insert(key, result);
                }
                result
            }
        }
    }

    /// Walk one declaration's parents.
    ///
    /// The oracle built here carries no [`ImportResolution`] — deliberately bare, unlike
    /// every oracle [`Self::type_of`] and friends hand out. With one attached,
    /// [`TypeScriptOracle::type_named_by`] would follow an imported alias to its declaration
    /// *inside this call*, across a file boundary this function never sees: the `Type` it
    /// hands back carries a symbol but no file, so the crossing would be invisible to
    /// [`Self::assignable`]'s own `at` tracking and the walk would silently lose the file it
    /// is really standing in. Left bare, the oracle reports the raw binding — imported or
    /// local, alias or not — and every crossing happens through `assignable`'s own
    /// `declaring`/`declared` resolution instead, which is the only place `at` is updated.
    fn heritage_assignable(
        &self,
        files: &FileAccess,
        at: (&FilePath, &tree_sitter::Tree, &str),
        declared: &str,
        target: (&FilePath, &str),
        depth: u32,
        walk: &mut Walk,
    ) -> Option<bool> {
        let (_, tree, source) = at;
        // The asking file is parsed by the *engine* and is deliberately not in the
        // declaration cache — re-reading it here would be a second parse of a file already
        // parsed, which is what `local/one-parser-per-file` exists to catch. So `declared_in`
        // runs directly over the tree this walk was already handed.
        let Some(declaration) =
            declared_in(self.support.resolver().as_ref(), tree, source, declared)
        else {
            // The name is not declared where the symbol said it was, which is a program this
            // provider could not read rather than one it read and rejected.
            return None;
        };
        // A parent the parser only partly read — an `ERROR` it recovered inside the body, or
        // a `MISSING` token it inserted — is unreadable, never a negative, the same reasoning
        // the walk's other `None`s carry. Read off the node, so the asking file's own tree
        // gets the same answer a declaration file's does.
        if declaration.has_error() {
            return None;
        }
        // Told when its own bound is what answered nothing. The walk threads the depth it has
        // already spent into `type_of_from` below, so the oracle can give up on `MAX_DEPTH`
        // several frames down and hand back a `None` that describes the path rather than the
        // alias — which `assignable` would then memoize against the declaration. One `Cell`
        // per call, living exactly as long as the oracle that writes it.
        let truncated = Cell::new(false);
        let oracle = TypeScriptOracle::new(&self.support, tree, source).with_exhaustion(&truncated);

        // An alias is transparent: `export type Money = Decimal` is `Decimal`. Read with
        // `type_of_from` rather than `type_named_by`: the latter always types the alias's
        // right-hand side in *nominal* position (`named_type`, which answers `Nominal` for
        // anything it cannot resolve, `number` included) and resets depth to zero on every
        // call, both wrong here. `type_of_from` types the value on its own terms — a
        // primitive right-hand side (`export type Amount = number`) comes back as
        // `Type::Primitive`, which `assignable`'s own `Type::Primitive(_) => Some(false)` arm
        // then answers honestly instead of failing to resolve a bare `number` as a nominal
        // name and returning `None` — and threads the depth this call has already spent
        // instead of restarting it, which `depth never resets` requires.
        if declaration.kind() == "type_alias_declaration"
            && let Some(value) = declaration.child_by_field_name("value")
        {
            if let Some(aliased) = oracle.type_of_from(value, depth) {
                return self.assignable(files, at, &aliased, target, depth.saturating_add(1), walk);
            }
            // Nothing came back. When the *oracle's* bound is why, the fall-through below is a
            // lie by construction: an alias declares no parents, so the loop answers
            // `Some(false)` for a chain that was never read to its end, and `assignable`
            // memoizes that against the alias — where a later, shallower reach of the same
            // alias reads it back instead of the `Some(true)` the walk would have produced.
            // Counted exactly as the walk's own truncation is, so the subtree stays unmemoized.
            if truncated.get() {
                walk.exhausted = walk.exhausted.saturating_add(1);
            }
        }

        let mut answer = Some(false);
        for parent in heritage_of(declaration) {
            let Some(parent_type) = oracle.type_named_by(parent) else {
                // A parent the oracle cannot name makes the whole walk unreadable, not
                // negative — the same reasoning the `symbol.is_none()` arm above uses.
                answer = None;
                continue;
            };
            match self.assignable(
                files,
                at,
                &parent_type,
                target,
                depth.saturating_add(1),
                walk,
            ) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => answer = None,
            }
        }
        answer
    }
}

/// Whether a specifier names something this resolver could read as TypeScript.
///
/// A bundler's project imports a stylesheet, a JSON asset and an image the same way it imports
/// a module. None of those is a module the oracle reads, all of them fail every probe, and
/// counting them makes `complete()` `false` for most files in a React codebase — where the
/// label then says "this project has CSS" rather than "a type answer is missing", which is the
/// one thing it exists to say.
///
/// **A denylist of asset extensions, never an allowlist of code ones**, because the two fail
/// in opposite directions and only one of the two failures is safe. An allowlist read
/// `./user.service` as an extension `service`, found it in no list of code extensions, and
/// skipped the import unprobed — so `complete()` answered `true` for a file whose imports were
/// never resolved, which is the one claim the flag must never make. That spelling is a
/// convention rather than a curiosity: `.service`, `.component`, `.module`, `.dto`, `.entity`,
/// `.guard`, `.pipe` and `.config` are how NestJS and Angular projects name most of their
/// files. A denylist that misses an asset kind costs eight absent probes and an honest
/// `complete() == false`; an allowlist that misses a naming convention costs a silent lie.
fn reads_as_code(specifier: &str) -> bool {
    let last = specifier.rsplit('/').next().unwrap_or(specifier);
    match last.rsplit_once('.') {
        // No extension at all is the ordinary spelling of a module.
        None => true,
        Some((_, extension)) => !ASSET_EXTENSIONS.contains(&extension),
    }
}

/// Extensions a bundler resolves that are not programs.
///
/// Stylesheets, data, images, fonts, prose, schemas and media — everything a loader turns into
/// a value without any of it being TypeScript. `.jsx` is deliberately **not** here: the
/// resolver strips it to the stem the way it strips `.js` (see `relative`), so a `.jsx`
/// specifier reaches a `.tsx` or `.ts` source, and one that reaches nothing is a real
/// incompleteness a file should be told about rather than an asset to skip over. `.tsx` is
/// resolved and parsed, so it belongs here no more than `.ts` does.
const ASSET_EXTENSIONS: &[&str] = &[
    "css", "scss", "sass", "less", "styl", "json", "svg", "png", "jpg", "jpeg", "gif", "webp",
    "avif", "ico", "woff", "woff2", "ttf", "eot", "otf", "md", "mdx", "txt", "yaml", "yml", "toml",
    "graphql", "gql", "wasm", "mp4", "webm", "mp3",
];

/// The type names one declaration extends or implements.
///
/// **Three different node shapes, and a walk that handles one is the obvious bug.** A class
/// carries `class_heritage` → `extends_clause`, whose `value` field is `"multiple": true`,
/// and optionally `class_heritage` → `implements_clause`, whose members carry no field name
/// at all; an interface carries `extends_type_clause` directly, whose `type` field is also
/// multiple. All three were read off `node-types.json` rather than off a sample.
fn heritage_of(declaration: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.named_children(&mut cursor) {
        match child.kind() {
            "class_heritage" => {
                let mut inner = child.walk();
                for clause in child.named_children(&mut inner) {
                    match clause.kind() {
                        "extends_clause" => collect_field(clause, "value", &mut out),
                        // A declared `implements` is a nominal relationship too — see
                        // `is_assignable_to`'s doc. `implements_clause` names its members
                        // with no field (`node-types.json` gives it `children`, not
                        // `fields`), unlike `extends_clause`'s `value`, so its types are
                        // walked as plain named children rather than through
                        // `collect_field`.
                        "implements_clause" => {
                            let mut types = clause.walk();
                            for interface in clause
                                .named_children(&mut types)
                                .filter(|child| child.kind() != "comment")
                            {
                                out.push(inner_type_name(interface));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "extends_type_clause" => collect_field(child, "type", &mut out),
            _ => {}
        }
    }
    out
}

/// Every child under one field name, which tree-sitter exposes one at a time.
fn collect_field<'t>(
    node: tree_sitter::Node<'t>,
    field: &str,
    out: &mut Vec<tree_sitter::Node<'t>>,
) {
    let mut cursor = node.walk();
    for child in node.children_by_field_name(field, &mut cursor) {
        out.push(inner_type_name(child));
    }
}

/// The bare name inside a possibly-generic type reference.
///
/// `Decimal<T>` parses as `generic_type` with a `name` field; type arguments are dropped
/// throughout this crate, so the name is what the walk follows.
fn inner_type_name(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    if node.kind() == "generic_type" {
        node.child_by_field_name("name").unwrap_or(node)
    } else {
        node
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
    /// Set once, anywhere in this call's recursion, the moment a hop finds `depth` already at
    /// `MAX_DEPTH`.
    ///
    /// A single `Option<Type>` cannot carry "the chain was cut" back through more than one
    /// level of recursion: `imported_alias_type` calls into a *nested* oracle, whose own
    /// `named_type` may call back into `imported_alias_type` several more times before the
    /// bound is finally spent, and every one of those intermediate frames sees only a plain
    /// `None` from the level below it — indistinguishable, on the type alone, from "this
    /// value simply could not be typed". Sharing one flag across every `Imports` built during
    /// one top-level call is what lets a frame several hops away from the exhaustion still
    /// answer [`Followed::Exhausted`](crate::oracle::Followed) rather than falling back to a
    /// nominal guess. Scoped to one call: each `TypeProvider` entry point starts a fresh
    /// `Cell`, so nothing here crosses calls, files, or worker threads.
    exhausted: &'a Cell<bool>,
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
            exhausted: self.exhausted,
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
    ) -> Followed {
        // The bound is checked here, before any work, rather than left to the nested oracle's
        // own check inside `type_of_from`: that check answers a bare `None`, and this frame
        // needs to say *why* there is no type, which only it can decide before recursing.
        if depth >= MAX_DEPTH {
            self.exhausted.set(true);
            return Followed::Exhausted;
        }
        let Some((decl, target)) = self.provider.imported(self.files, from, module, name) else {
            return Followed::NotAnAlias;
        };
        let Some(node) = target_node(&decl, &target.name) else {
            return Followed::NotAnAlias;
        };
        // Only an alias. A class or an interface keeps its use-site symbol, which is what the
        // caller does on `NotAnAlias` — see `ImportResolution`'s own documentation.
        if node.kind() != "type_alias_declaration" {
            return Followed::NotAnAlias;
        }
        let Some(value) = node.child_by_field_name("value") else {
            return Followed::NotAnAlias;
        };
        let nested = Imports {
            provider: self.provider,
            files: self.files,
            exhausted: self.exhausted,
        };
        let oracle = TypeScriptOracle::new(&self.provider.support, &decl.tree, &decl.source)
            .with_imports(&decl.path, &nested);
        match oracle.type_of_from(value, depth) {
            Some(ty) => Followed::Type(ty),
            // `self.exhausted` may have been set by a hop deeper than this one — the walk
            // that just returned `None` can be several files past where the bound was
            // actually spent, and this is the only place that flag is read back.
            None if self.exhausted.get() => Followed::Exhausted,
            None => Followed::NotAnAlias,
        }
    }

    fn imported_return_type(
        &self,
        from: &FilePath,
        module: &str,
        name: &ImportedName,
        depth: u32,
    ) -> Option<Type> {
        let (decl, target) = self.provider.imported(self.files, from, module, name)?;
        let node = target_node(&decl, &target.name)?;
        let nested = Imports {
            provider: self.provider,
            files: self.files,
            exhausted: self.exhausted,
        };
        let oracle = TypeScriptOracle::new(&self.provider.support, &decl.tree, &decl.source)
            .with_imports(&decl.path, &nested);
        oracle.return_type_from(node, depth)
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
        let exhausted = Cell::new(false);
        let imports = Imports {
            provider: self,
            files: q.files,
            exhausted: &exhausted,
        };
        self.oracle_with(&q, &imports).type_of(q.node)
    }

    fn symbol_of(&self, q: Query<'_>) -> Option<Symbol> {
        let exhausted = Cell::new(false);
        let imports = Imports {
            provider: self,
            files: q.files,
            exhausted: &exhausted,
        };
        self.oracle_with(&q, &imports).symbol_of(q.node)
    }

    fn return_type_of(&self, q: Query<'_>) -> Option<Type> {
        let exhausted = Cell::new(false);
        let imports = Imports {
            provider: self,
            files: q.files,
            exhausted: &exhausted,
        };
        self.oracle_with(&q, &imports).return_type_of(q.node)
    }

    /// Whether the type at `q.node` is the type `module` exports as `name`, or declares a
    /// relationship to it — `extends` or `implements` — across files, through aliases of the
    /// named type. See [`TypeProvider::is_assignable_to`] for the full contract, including
    /// its narrowings (declaration merging, a generic annotation at the use site, and an
    /// unexported target name) and its documented gap (no function-local scoping — a
    /// shadowing declaration inside a function is not distinguished from the top-level one).
    fn is_assignable_to(&self, q: Query<'_>, module: &str, name: &str) -> Option<bool> {
        // Resolved once: the file and name `(module, name)` designates, so `assignable` has a
        // fixed target to compare a declaring file against however many files the walk
        // crosses. See `assignable`'s own documentation for why a per-step string comparison
        // against `module`/`name` cannot do this job.
        let entry = resolve_specifier(q.files, q.file, module)?;
        let target = self.export_target(q.files, &entry, name)?;

        // Bare, for the same reason `heritage_assignable`'s oracle is: `type_of` on a
        // `type_annotation` would otherwise follow an imported alias to its declaration
        // before `assignable` ever sees the type, crossing a file boundary `assignable`'s
        // own `at` tracking never learns about.
        let ty = TypeScriptOracle::new(&self.support, q.tree, q.source).type_of(q.node)?;
        // Fresh per call: nothing here crosses calls, files or worker threads.
        let mut walk = Walk {
            visiting: BTreeMap::new(),
            answers: BTreeMap::new(),
            lowlink: usize::MAX,
            exhausted: 0,
        };
        self.assignable(
            q.files,
            (q.file, q.tree, q.source),
            &ty,
            (&target.file, &target.name),
            0,
            &mut walk,
        )
    }

    /// Whether every import in `q`'s file resolved to a declaration this provider could read.
    ///
    /// Eager rather than lazy: every specifier is resolved here, not only the ones a rule
    /// happened to ask about, because a miss has to be recorded as a dependency even when
    /// nothing went looking for the type behind it — the cache's own read on this file must
    /// see every candidate path an import could have named, so that a declaration appearing
    /// later invalidates a rule that stayed silent for its absence. Memoized per file, since
    /// several rules ask the same question about the same file within one run.
    ///
    /// One thing it deliberately does not count: a specifier that is not code — `./app.css`,
    /// `./data.json`, `./logo.svg` — is skipped entirely, probes and all. It is not a module
    /// this oracle reads, and counting it would label most of a bundler's project incomplete
    /// for having stylesheets.
    ///
    /// **The contract is resolve-and-parse, judged per declaration where one is reached.**
    /// `tree_sitter::Parser::parse` answers a tree for any UTF-8 input, so a file this
    /// provider could not fully read shows up only as parse faults — `ERROR` nodes, and the
    /// `MISSING` tokens an unclosed brace leaves — and the whole-file verdict this once asked
    /// let one damaged statement mark every importer of the file incomplete, project-wide,
    /// throwing away the declarations outside the damaged span that answer normally (#229).
    /// Each named import is walked to the node that declares it, through re-exports like
    /// every other arm, and the name is unread when a link of that chain could not be read:
    /// a specifier that resolves to nothing, a file that will not parse, a reached
    /// declaration whose own subtree the parser did not finish (`has_error()` on the node,
    /// which counts both kinds of fault), or a file in a dialect this provider has no grammar
    /// for, which it does not read at all. A walk that ends on a *clean* module with no
    /// export it can model is not unread — `export = X` beside `declare namespace X` is that
    /// shape for every member of `X`, and counting it silenced every rule on every file
    /// naming one (the #232 review). A nameless import — a side-effect one, or a namespace
    /// binding, or `export *` — has no single node to reach: a side-effect import asserts the
    /// module's whole shape and a namespace import binds a module object whose members can
    /// be anything, so both keep the whole-file verdict.
    fn complete(&self, q: Query<'_>) -> bool {
        if let Some(known) = self.completeness().get(q.file) {
            return *known;
        }

        let mut complete = true;
        for imported in imports_with_names(q.tree, q.source) {
            // A stylesheet, a JSON asset or an image is not a module this oracle reads, and a
            // bundler's `import './app.css'` is not a missing type answer — see `reads_as_code`.
            // Skipped before the probe rather than after it, so nothing about it is recorded
            // either: eight absent reads per such import, on a codebase where most files have
            // one, is cache-entry size spent on a question nobody asked.
            if !reads_as_code(&imported.specifier) {
                continue;
            }
            // Resolved once per specifier, before the name loop: which reads the *specifier*
            // itself records must not depend on how many names share the module. (The
            // per-name chains below add their own reads — that is the pass recording what it
            // really consulted; the access memo keeps a repeated path from being recorded
            // twice.)
            let Some(file) = resolve_specifier(q.files, q.file, &imported.specifier) else {
                complete = false;
                continue;
            };
            // A dialect this provider has no grammar for is not read, named or nameless —
            // see `reads_dialect_of` — and nothing is parsed to find that out.
            if !self.reads_dialect_of(file.as_str()) {
                complete = false;
                continue;
            }
            let Some(decl) = self.declaration(q.files, &file) else {
                // A specifier that names a file this provider cannot parse is exactly as
                // partial as one that names nothing: either way no answer about a name from
                // that module was reached by reading anything.
                complete = false;
                continue;
            };
            // Nameless: no single declaration to reach, so whatever the parse carries
            // counts. The namespace arm of a mixed clause (`import d, * as ns`) is judged
            // the same way, because the module object it binds reaches everywhere.
            if imported.names.is_empty() || imported.names.contains(&ImportedName::Namespace) {
                if decl.has_error {
                    complete = false;
                }
                continue;
            }
            for name in &imported.names {
                let wanted = match name {
                    ImportedName::Named(exported) => exported.as_str(),
                    ImportedName::Default => "default",
                    // Handled with the nameless arm above; unreachable from the enumeration
                    // `imports_with_names` does, and the nameless reading is what a
                    // namespace binding means if one ever arrives here.
                    ImportedName::Namespace => continue,
                };
                // The contract is resolve-and-parse, per declaration where one is reached.
                // A name is unread when a link of its chain could not be read — a specifier
                // that resolves to nothing, a file that will not parse, a reached
                // declaration the parser did not finish — and *not* when a clean module
                // simply has no export the walk can model. `export = X` beside `declare
                // namespace X` is that second case for every member of `X`, and it is the
                // shape most `@types` packages ship: counting it silenced every rule on every
                // file naming one of their members, which is what the #232 review found.
                if matches!(
                    self.export_walk(q.files, &file, wanted),
                    Err(Unreached::Unread)
                ) {
                    complete = false;
                }
            }
        }

        self.completeness().insert(q.file.clone(), complete);
        complete
    }

    /// Start a run, and answer no key term.
    ///
    /// **Only `completeness` is cleared here.** It carries no hash to compare against — a
    /// verdict over a whole file's imports, not a single read — so a provider held across
    /// requests (#191) would otherwise answer a second run from the first run's filesystem: a
    /// file whose imports did not resolve would stay incomplete forever. `declarations` is
    /// *not* cleared: it is keyed by content hash, [`Self::declaration`] compares that hash on
    /// every access, and a stale entry is therefore never served whether or not this method
    /// touched it. Clearing it here would only cost the parse back — and for a held provider,
    /// re-paying that cost every request is the exact overhead holding the provider exists to
    /// remove. [`Self::revalidate`] is what drops a hash-mismatched entry proactively, ahead
    /// of `declaration()` finding out the hard way.
    ///
    /// The file list is never asked for: this provider's dependencies are the tracked reads
    /// on each entry, so there is nothing to build up front. Answering an empty term is what
    /// keeps the builtin provider out of `analysis_hash`'s `programs` field.
    fn begin_run(
        &self,
        files: &dyn Fn() -> Vec<FilePath>,
        budget: AnalysisBudget,
    ) -> Result<Vec<u8>, BeginRunError> {
        // Neither is read: there is nothing to build up front, so there is nothing for a
        // budget to bound either.
        let _ = (files, budget);
        self.completeness().clear();
        Ok(Vec::new())
    }

    fn identity(&self) -> Vec<u8> {
        // Tagged as well as hashed. `oracle_identity` alone would let a future provider that
        // happened to derive its identity the same way collide with this one, and the tag is
        // what makes "which provider answered" part of the key rather than an inference.
        //
        // After the tag, the main grammar's shape digest, then the tsx grammar's behind a
        // presence byte, then the resolver's analysis identity: the digests say *which*
        // grammar parses each dialect — `TypeScript` and `Tsx` share one analysis identity,
        // so that term alone could not tell a provider over one from a provider over the
        // other — and the presence byte, with it and only with it, carries a second grammar,
        // so a provider built with one can never fold to the same bytes as one built without
        // it. A key that cannot tell two runs apart lets one warm the other's cache. The
        // oracle's identity stays last, the one field every provider over every grammar pair
        // carries. `the_identity_folds_both_grammar_digests_and_the_resolver` pins the
        // layout byte for byte, because a test of inequality alone is satisfied by the
        // vectors' lengths.
        let mut out = Vec::with_capacity(8 + 32 + 1 + 32 + 32 + 32);
        out.extend_from_slice(b"builtin:");
        out.extend_from_slice(&self.grammar_digest);
        if let Some(tsx) = &self.tsx {
            out.push(1);
            out.extend_from_slice(&tsx.grammar_digest);
        }
        out.extend_from_slice(&self.analysis_identity);
        out.extend_from_slice(&crate::oracle_identity());
        out
    }

    fn revalidate(&self, files: &FileAccess) {
        // Every held declaration is keyed by the content hash it was parsed from; one whose
        // bytes moved, or which is gone, is dropped and re-parsed on its next `declaration()`
        // call. Completeness carries no hash to compare against — it is a verdict over a
        // whole file's imports, not a single read — so it is simply forgotten, the same
        // coarse-but-correct move `begin_run` already makes for it.
        self.declarations().retain(|path, decl| {
            matches!(files.hash_of(path.as_str()), Ok(Some(hash)) if hash == decl.hash)
        });
        self.completeness().clear();
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

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language as _;
    use lanekeep_lang_js::{Tsx, TypeScript};

    use super::{
        AnalysisBudget, BuiltinProvider, FileAccess, FilePath, Query, Type, TypeProvider,
        extension_is_tsx,
    };
    use crate::types::Primitive;

    /// Parse `source` with the TypeScript grammar, for building a `Query` by hand.
    ///
    /// A local copy of `tests/provider.rs`'s helper of the same name: that one is compiled
    /// into a separate integration-test binary and cannot be reached from a unit test, which
    /// is exactly what the parse-count seam below needs — it is a private field.
    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("the TypeScript grammar loads");
        parser.parse(source, None).expect("the source parses")
    }

    /// The last node of `kind` in the tree, in source order — a use rather than a declaration.
    fn last_of<'t>(tree: &'t tree_sitter::Tree, kind: &str) -> tree_sitter::Node<'t> {
        let mut best: Option<tree_sitter::Node<'t>> = None;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == kind && best.is_none_or(|b| node.start_byte() > b.start_byte()) {
                best = Some(node);
            }
            let mut cursor = node.walk();
            let children: Vec<tree_sitter::Node<'t>> = node.children(&mut cursor).collect();
            stack.extend(children);
        }
        best.unwrap_or_else(|| panic!("no `{kind}` node in the tree"))
    }

    /// A budget generous enough that nothing here can breach it.
    fn budget() -> AnalysisBudget {
        AnalysisBudget::start(std::time::Duration::from_mins(10))
    }

    /// The tsx grammar is a second parser with a second identity, and the provider's own
    /// identity is what a cache key folds — so a run whose provider can read `.tsx` must not
    /// share one with a run whose provider cannot.
    #[test]
    fn a_tsx_parser_moves_the_provider_identity() {
        let without = BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let with =
            BuiltinProvider::probe_with(&TypeScript, Some(&Tsx)).expect("TypeScript and tsx");
        assert_ne!(
            without.identity(),
            with.identity(),
            "the tsx grammar's identity is part of the provider's"
        );
    }

    /// Which grammar parses `.ts` is part of the key, not only whether a second one exists:
    /// `TypeScript` and `Tsx` share one `analysis_identity`, so folding that alone let a
    /// provider over the TSX grammar warm the cache of one over the TypeScript grammar.
    #[test]
    fn the_main_grammar_moves_the_provider_identity() {
        let over_typescript =
            BuiltinProvider::probe_with(&TypeScript, Some(&Tsx)).expect("TypeScript and tsx");
        let over_tsx = BuiltinProvider::probe_with(&Tsx, Some(&Tsx)).expect("tsx twice");
        assert_ne!(
            over_typescript.identity(),
            over_tsx.identity(),
            "two main grammars over one tsx grammar are two providers"
        );
    }

    /// The fold, byte for byte: a test that only asserts inequality is satisfied by the
    /// vectors' lengths alone, and survived a fold that pushed zeros for the tsx digest.
    #[test]
    fn the_identity_folds_both_grammar_digests_and_the_resolver() {
        let with =
            BuiltinProvider::probe_with(&TypeScript, Some(&Tsx)).expect("TypeScript and tsx");
        let expected = [
            &b"builtin:"[..],
            &lanekeep_lang::grammar_digest(&TypeScript.grammar()),
            &[1],
            &lanekeep_lang::grammar_digest(&Tsx.grammar()),
            &TypeScript.analysis_identity(),
            &crate::oracle_identity(),
        ]
        .concat();
        assert_eq!(with.identity(), expected);
        let without = BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let expected = [
            &b"builtin:"[..],
            &lanekeep_lang::grammar_digest(&TypeScript.grammar()),
            &TypeScript.analysis_identity(),
            &crate::oracle_identity(),
        ]
        .concat();
        assert_eq!(without.identity(), expected);
    }

    /// The parser selector agrees with the registry about what a `.tsx` path is — the
    /// extension, case-insensitively, and nothing else about the name.
    #[test]
    fn a_tsx_extension_is_the_last_component_dot_tsx() {
        assert!(extension_is_tsx("src/Button.tsx"));
        assert!(extension_is_tsx("node_modules/w/src/Button.TSX"));
        assert!(!extension_is_tsx("src/Button.ts"));
        assert!(!extension_is_tsx("src/v1.2/Button"));
        assert!(
            !extension_is_tsx("src/.tsx"),
            "a hidden file has no extension"
        );
        assert!(!extension_is_tsx(".tsx"), "nor does one at the root");
        assert!(
            !extension_is_tsx("Button.tsx/index"),
            "the extension is the last component's"
        );
    }

    /// The mirror of `a_path_that_was_absent_is_parsed_once_it_becomes_text`: a path that has
    /// stopped answering does not keep its parse.
    ///
    /// A unit test rather than an integration one because the only thing it can observe is the
    /// size of a private map — the *answer* is `None` either way, which is exactly why holding
    /// the entry was invisible. What it costs is a whole declaration file's tree and source
    /// held until the next `begin_run`, on a path nothing can ever be served from again.
    #[test]
    fn a_declaration_whose_file_vanished_is_dropped() {
        let dir =
            std::env::temp_dir().join(format!("lanekeep-builtin-vanished-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");
        std::fs::write(dir.join("lib.d.ts"), "export declare class Big {}\n")
            .expect("writes the declaration file");

        let provider = BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let path = FilePath::new("lib.d.ts");
        assert!(
            provider
                .declaration(&FileAccess::new(&dir), &path)
                .is_some(),
            "it is there and it parses"
        );
        assert_eq!(provider.declarations().len(), 1, "so it is held");

        std::fs::remove_file(dir.join("lib.d.ts")).expect("removes the declaration file");
        assert!(
            provider
                .declaration(&FileAccess::new(&dir), &path)
                .is_none(),
            "nothing is there now"
        );
        assert_eq!(
            provider.declarations().len(),
            0,
            "and the parse it can no longer serve is not held either"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `revalidate` drops only the entry whose bytes moved, and clears completeness wholesale.
    ///
    /// Two declaration files are parsed and memoized; one is rewritten between calls. The
    /// changed entry is dropped — a stale parse must not be served again — and the unchanged
    /// one is kept, which is the whole point of holding a provider across requests (#191):
    /// revalidation that dropped everything would cost exactly what never holding it at all
    /// costs.
    #[test]
    fn revalidate_drops_only_the_rewritten_declaration() {
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-builtin-revalidate-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");
        std::fs::write(
            dir.join("stable.d.ts"),
            "export declare const rate: number;\n",
        )
        .expect("writes the stable declaration file");
        std::fs::write(
            dir.join("moved.d.ts"),
            "export declare const rate: number;\n",
        )
        .expect("writes the declaration file that will move");

        let provider = BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let files = FileAccess::new(&dir);
        let stable = FilePath::new("stable.d.ts");
        let moved = FilePath::new("moved.d.ts");
        assert!(provider.declaration(&files, &stable).is_some());
        assert!(provider.declaration(&files, &moved).is_some());
        assert_eq!(provider.declarations().len(), 2, "both are held");
        // A file completeness would have been decided over, so the clearing this test also
        // asserts has something in it to clear.
        provider
            .completeness()
            .insert(FilePath::new("src/a.ts"), true);

        std::fs::write(
            dir.join("moved.d.ts"),
            "export declare const rate: string;\n",
        )
        .expect("rewrites the declaration file");
        provider.revalidate(&FileAccess::new(&dir));

        assert_eq!(
            provider.declarations().len(),
            1,
            "the rewritten entry is dropped, the unchanged one is not"
        );
        assert!(
            provider.declarations().contains_key(&stable),
            "the file whose bytes did not move is still held"
        );
        assert!(
            !provider.declarations().contains_key(&moved),
            "the file whose bytes moved is not"
        );
        assert!(
            provider.completeness().is_empty(),
            "completeness carries no hash to compare against, so it is simply forgotten"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of holding a provider: `begin_run` no longer throws its parses away, and a
    /// held declaration answers across two runs without being read from disk a second time —
    /// but a rewrite between them is still caught, because `revalidate` is what a session
    /// calls to catch it.
    #[test]
    fn a_held_declaration_survives_begin_run_and_is_reparsed_after_a_revalidated_rewrite() {
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-builtin-parse-once-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");
        std::fs::write(
            dir.join("money.d.ts"),
            "export declare const rate: number;\n",
        )
        .expect("writes the declaration file");

        let provider = BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let subject = "import { rate } from './money';\nconst y = rate;\n";
        let tree = parse(subject);
        let file = FilePath::new("a.ts");
        let node = last_of(&tree, "identifier");

        // Each "request" below builds its own `FileAccess`, exactly as `SessionProvider` does
        // per request in `crates/lanekeep-cli/src/session.rs` — a `FileAccess` memoizes the
        // hashes it reads for its own lifetime, so reusing one across requests would hide a
        // rewrite behind that memo rather than testing what `begin_run`/`revalidate` do.
        let request_one = FileAccess::new(&dir);
        assert_eq!(
            provider.type_of(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &request_one,
            }),
            Some(Type::Primitive(Primitive::Number)),
            "the first request reads and parses the declaration file"
        );
        assert_eq!(provider.parses(), 1, "one read, one parse");

        provider
            .begin_run(&Vec::new, budget())
            .expect("a second run begins");
        let request_two = FileAccess::new(&dir);
        assert_eq!(
            provider.type_of(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &request_two,
            }),
            Some(Type::Primitive(Primitive::Number)),
            "still answers across the run boundary"
        );
        assert_eq!(
            provider.parses(),
            1,
            "the declaration is held across `begin_run` now — its bytes did not move, so it \
             is not parsed again"
        );

        std::fs::write(
            dir.join("money.d.ts"),
            "export declare const rate: string;\n",
        )
        .expect("rewrites the declaration file");
        let request_three = FileAccess::new(&dir);
        provider.revalidate(&request_three);
        provider
            .begin_run(&Vec::new, budget())
            .expect("a third run begins");
        assert_eq!(
            provider.type_of(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &request_three,
            }),
            Some(Type::Primitive(Primitive::String)),
            "revalidate dropped the stale entry, so the rewrite is seen"
        );
        assert_eq!(
            provider.parses(),
            2,
            "the rewritten file is re-parsed exactly once, on the request that revalidated it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
