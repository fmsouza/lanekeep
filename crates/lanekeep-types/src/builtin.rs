//! The bounded provider: this crate's own oracle, plus the files it is allowed to open.
//!
//! Run-scoped state lives here rather than on [`TypeScriptOracle`](crate::TypeScriptOracle),
//! which owns exactly one parse and must stay that way. What this holds is a parser, a cache
//! of parsed declaration files and the memo of paths that were not there — so a library's
//! `.d.ts` is parsed once per run whatever imports it, and a miss is not re-probed per query.
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
    import_specifiers, target_node,
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
    /// One parser for everything this provider opens, behind a lock.
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
    /// Declaration files parsed so far this run, by path.
    ///
    /// A `BTreeMap`, per the ordering invariant, and behind a lock because rayon runs one
    /// worker per file and they share this provider. A library's `.d.ts` is parsed once
    /// whatever imports it, which is the difference between a 500 KB `typescript.d.ts` costing
    /// tens of milliseconds once and costing them per importing file.
    ///
    /// Entries carry the hash their bytes had, and [`Self::declaration`] compares it against
    /// what the *asking* access read — see that method for why serving by path alone writes an
    /// entry describing neither version of a file rewritten mid-run.
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
}

impl fmt::Debug for BuiltinProvider {
    /// Hand-written because neither `TypeScriptSupport` nor `tree_sitter::Parser` is
    /// `Debug`, the same reason and the same shape as the oracle's own impl.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinProvider").finish_non_exhaustive()
    }
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
            completeness: Mutex::new(BTreeMap::new()),
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

    /// The parsed declaration file at `path`, parsed once per run per version of its bytes.
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
        let parsed = Declaration::parse(path.clone(), source, &mut self.parser())?;
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

    /// Whether `ty`, read in the file `(tree, source)` is the parse of, is the type declared
    /// at `target`.
    ///
    /// Nominal, never structural. `Some(false)` is a real answer — the walk completed and
    /// reached nothing — and `None` is "a link in the chain could not be read", which a rule
    /// must not treat as a negative: a project whose `node_modules` is absent would otherwise
    /// have every governed value reported.
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
                // ambient global (`Date`, never declared or imported anywhere) — and,
                // indistinguishably from the resolver's own output, a name the resolver
                // cannot bind for a reason unrelated to whether it exists:
                // `lanekeep-lang-js`'s `declaration_entry_of` has no arm for
                // `interface_declaration`, `enum_declaration`, `abstract_class_declaration`,
                // `module`, `internal_module`, or the `ambient_declaration` wrapper `.d.ts`
                // files write `declare` as — so `interface Amountish {}` and
                // `export declare class Decimal {}` are as invisible to it as an undeclared
                // global would be.
                //
                // `declared_in` is the fallback that tells the two apart: the same
                // ambient-aware, resolver-free walk `declarations.rs` uses for every `.d.ts`
                // lookup in this crate, over the *current* file. It sees every one of the
                // kinds the resolver misses. Only when that also finds nothing is this
                // genuinely unreadable — matching `symbol_at`'s own contract, which already
                // returns `None` outright rather than a `Symbol` with empty fields.
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
                    declared_in(tree, source, written)?;
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
        let Some(declaration) = declared_in(tree, source, declared) else {
            // The name is not declared where the symbol said it was, which is a program this
            // provider could not read rather than one it read and rejected.
            return None;
        };
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
/// files. A denylist that misses an asset kind costs six absent probes and an honest
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
/// a value without any of it being TypeScript. `.tsx` and `.jsx` are deliberately **not** here:
/// the resolver refuses them (see `RELATIVE_SUFFIXES`), and that refusal is a real
/// incompleteness a file should be told about rather than an asset to skip over.
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
    /// Two things it deliberately does not count. A specifier that is not code — `./app.css`,
    /// `./data.json`, `./logo.svg` — is skipped entirely, probes and all: it is not a module
    /// this oracle reads, and counting it would label most of a bundler's project incomplete
    /// for having stylesheets. And a declaration that resolves but whose *parse carries an
    /// `ERROR`* counts as unread: `tree_sitter::Parser::parse` answers a tree for any UTF-8
    /// input, so the names inside the broken span answer `undefined` while the ones outside it
    /// answer normally — a partial answer with nothing on it to say so, which is the one
    /// combination a rule cannot defend itself against.
    fn complete(&self, q: Query<'_>) -> bool {
        if let Some(known) = self.completeness().get(q.file) {
            return *known;
        }

        let mut complete = true;
        for specifier in import_specifiers(q.tree, q.source) {
            // A stylesheet, a JSON asset or an image is not a module this oracle reads, and a
            // bundler's `import './app.css'` is not a missing type answer — see `reads_as_code`.
            // Skipped before the probe rather than after it, so nothing about it is recorded
            // either: six absent reads per such import, on a codebase where most files have
            // one, is cache-entry size spent on a question nobody asked.
            if !reads_as_code(&specifier) {
                continue;
            }
            // Resolved, readable *and* parsed without error. A specifier that names a file
            // this provider cannot parse is exactly as partial as one that names nothing:
            // either way no answer about a name from that module was reached by reading
            // anything. A file that parses only partly is the worse case of the two, because
            // the names outside the `ERROR` span still answer and the ones inside it are
            // silently absent.
            if resolve_specifier(q.files, q.file, &specifier)
                .and_then(|file| self.declaration(q.files, &file))
                .is_none_or(|decl| decl.has_error)
            {
                complete = false;
            }
        }

        self.completeness().insert(q.file.clone(), complete);
        complete
    }

    /// Start a run cold, and answer no key term.
    ///
    /// Both memos are keyed by path with nothing beside them — see their field documentation
    /// — so a provider held across requests (#191) would otherwise answer a second run from
    /// the first run's filesystem: a file whose imports did not resolve would stay incomplete
    /// forever. Clearing is coarse and certainly correct; dropping by hash is plan 6's
    /// refinement, and `completeness` carries no hash to drop by today.
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
        self.declarations().clear();
        self.completeness().clear();
        Ok(Vec::new())
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

#[cfg(test)]
mod tests {
    use lanekeep_lang_js::TypeScript;

    use super::{BuiltinProvider, FileAccess, FilePath};

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
}
