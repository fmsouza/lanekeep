//! Module resolution and loading for rule files.
//!
//! Rules are ES modules. They may import from `lanekeep` and from each other; nothing else
//! resolves. There is no `node_modules` lookup, no bare-specifier resolution, and no way to
//! reach a file outside the rules root.
//!
//! # Confinement
//!
//! The rules root is canonicalized once at construction, and every resolved module is
//! canonicalized and checked against it. Canonicalizing rather than comparing strings is
//! what makes the check hold against symlinks: a link inside the root pointing at
//! `/etc/passwd` resolves to a path outside the root and is rejected, where a lexical
//! comparison would see an innocent-looking relative path and allow it.
//!
//! Traversal is also rejected lexically, before touching the filesystem, so `../../secrets`
//! produces a message about escaping the root rather than a confusing "not found".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use lanekeep_core::files::normalize;
use lanekeep_lang::Language;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::{Declared, Module};
use rquickjs::{Ctx, Error as JsError};
use thiserror::Error;

use crate::typescript::strip_types;

/// The specifier that resolves to lanekeep's own module.
pub const HOST_MODULE: &str = "lanekeep";

/// The host module.
///
/// `defineRule` and `defineConfig` are identity functions, and that is not a placeholder —
/// it is what they are. Their entire purpose is to give the TypeScript compiler something
/// to infer against in the author's editor, which costs nothing at runtime.
const HOST_MODULE_SOURCE: &str = r"
    export function defineRule(rule) { return rule; }
    export function defineConfig(config) { return config; }
";

/// Resolves a built-in rule name to its embedded source.
///
/// A function rather than a dependency, so this crate stays unaware of which rules ship —
/// `lanekeep-js` sits below `lanekeep-rules`, and reaching upward for them would invert the
/// layering for no gain.
pub type BuiltinSource = fn(&str) -> Option<&'static str>;

/// Resolves a built-in rule name to its embedded component.
///
/// The sibling of [`BuiltinSource`], and it lives here for one reason: **`lanekeep/<name>` has
/// to mean one thing.** A built-in shipped as a component is not importable, and the resolver
/// is what has to say so — a name it did not know about would come back "no built-in rule by
/// that name", which is what a typo looks like and would send a reader hunting for a
/// misspelling that is not there.
///
/// Nothing in this crate loads the bytes. `lanekeep-config` reads them through
/// [`RuleRoot::builtin_component`] when a config names one, exactly as it reads a `.wasm` path.
/// Keeping both lookups on one value is what stops a name resolving to a module in one place
/// and a component in another.
///
/// **The index is part of the answer.** One artifact hosts several rules — the four TypeScript
/// built-ins share one — so bytes alone name a program rather than a rule. A lookup returning
/// only bytes would leave the caller to run whichever rule the component enumerates first, and
/// a wrong rule reporting is indistinguishable from a right one reporting.
pub type BuiltinComponent = fn(&str) -> Option<(&'static [u8], u32)>;

/// Resolves a built-in rule name to its component's embedded source map.
///
/// A third hook rather than a third element of [`BuiltinComponent`], because it answers a
/// different kind of question and almost every caller has no use for it: what a map buys is where
/// a *thrown* rule error is reported, and nothing else. A violation's position never passes
/// through JavaScript at all, so a build that wired this to nothing would produce identical
/// violations and a worse stack.
///
/// That is also its risk, and the reason `crates/lanekeep-rules/tests/source_maps.rs` asserts the
/// wiring end to end rather than trusting it: a caller that sets [`RuleRoot::with_builtin_components`]
/// and forgets this one gets rules that work and diagnostics that name `entry.js`, with nothing
/// anywhere going red.
///
/// `None` is the ordinary answer. Every component built from Rust is one — a panicking rule traps,
/// and a trap reaches the host with no stack to remap.
pub type BuiltinComponentMap = fn(&str) -> Option<&'static [u8]>;

/// The longest a component-hosted built-in's name may be before refusing it stops being
/// actionable.
///
/// Not a limit on rule names in general, and nothing enforces it here — it is a budget derived
/// from one number this crate does not control. [`ResolveError::NotAModule`] reaches a user
/// through QuickJS, which truncates a thrown error at 255 bytes, behind rquickjs's
/// `Error resolving module '<specifier>' from '<path>': ` framing. The name is spent twice in
/// that — once in the specifier, once inside the message — so each character costs two bytes of
/// whatever is left for the *project's own path*, and what gets cut is the end of the message,
/// which is the half telling the user what to do.
///
/// The name is the *rule's*, as a config writes it, not the artifact's: `typescript-builtins`
/// hosts four rules and appears in no message.
///
/// # It was 15, and 21 is what the TypeScript built-ins cost
///
/// The four rules compiled into `typescript-builtins` are 17 to 21 characters —
/// `no-restricted-imports` is the longest thing that can appear here — and the arithmetic is
/// `35 + (9 + name) + path + (61 + name) <= 255`, so `path <= 150 - 2 * name`. The whole
/// message survived beside a **120**-byte config path at 15 and survives beside a **108**-byte
/// one at 21.
///
/// **That is a smaller path budget, accepted deliberately and stated here**, which is one of
/// the two honest ways this constant may move; the other is shortening the message, which costs
/// a user the wording that tells them what to do. 108 bytes still clears
/// `/Users/alice/projects/acme/packages/checkout/lanekeep.config.ts` — 63 — with 45 to spare,
/// and it does not clear everything: at 27 characters `no-mutable-default-argument` would leave
/// 96, so migrating *that* rule is a decision to take here rather than a table edit.
///
/// `the_refusal_survives_quickjs_beside_a_long_path` derives it and `lanekeep-rules`'
/// `every_component_name_fits_the_refusal_message` enforces it against the names that actually
/// ship, because this crate sits below that one and cannot see them.
pub const MAX_COMPONENT_NAME: usize = 21;

/// The default: no built-ins, so a bare `lanekeep-js` resolves only project modules.
fn no_builtins(_name: &str) -> Option<&'static str> {
    None
}

/// The default: no built-in components.
fn no_builtin_components(_name: &str) -> Option<(&'static [u8], u32)> {
    None
}

/// The default: no source maps, which is also the answer for every component that has none.
fn no_builtin_component_maps(_name: &str) -> Option<&'static [u8]> {
    None
}

/// The prefix a built-in specifier carries, as in `lanekeep/no-default-export`.
const BUILTIN_PREFIX: &str = "lanekeep/";

/// Extensions tried for a specifier that does not name one, in order.
const EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs"];

/// Why a module specifier could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// A bare specifier, which would be an npm package.
    #[error(
        "cannot import `{specifier}`\n  \
         rule modules run in a sandbox with no package resolution, so only `lanekeep` and \
         relative paths starting with `./` or `../` can be imported\n  \
         if this needs a package, inline what you need from it instead"
    )]
    BareSpecifier {
        /// The specifier as written.
        specifier: String,
    },

    /// The specifier resolves outside the rules root.
    #[error(
        "cannot import `{specifier}`\n  \
         it resolves outside the rules directory, and rule modules may only import from \
         within it"
    )]
    EscapesRoot {
        /// The specifier as written.
        specifier: String,
    },

    /// Nothing exists at the specifier.
    #[error("cannot find module `{specifier}`\n  tried: {tried}")]
    NotFound {
        /// The specifier as written.
        specifier: String,
        /// The candidate paths that were tried.
        tried: String,
    },

    /// The module exists but could not be read.
    #[error("cannot read module `{path}`: {detail}")]
    Unreadable {
        /// The path that failed.
        path: String,
        /// The underlying reason.
        detail: String,
    },

    /// The specifier names a built-in that ships as a component rather than as a module.
    ///
    /// Its own variant rather than a [`ResolveError::NotFound`] with a different string,
    /// because the two are different facts and only one of them is the user's mistake. A
    /// built-in that is not there is a typo; a built-in that is a component is spelled
    /// correctly and simply cannot be imported, and telling a reader to check their spelling
    /// would send them looking for something that is not wrong.
    ///
    /// **One line, carrying both the fact and the remedy, and that is not a style choice.**
    /// QuickJS truncates a thrown error at 255 bytes, and a resolution failure reaches the user
    /// as `Error resolving module '<specifier>' from '<absolute path>': <this>` — 35 bytes of
    /// framing plus the specifier plus the *project's own path* before this message starts. So
    /// a second line is not a place to put anything: it is the first thing a real path spends
    /// the budget on.
    ///
    /// A two-line version of this shipped and was wrong in exactly that way. It front-loaded
    /// the fact and put "name it in a `lanekeep.json`" second, which survived only for a config
    /// path of 47 characters or fewer — so every user deep enough in a monorepo to have the
    /// problem was told they had it and not what to do. There is no in-format escape either:
    /// `packages/lanekeep/index.d.ts` types `rules` as `Rule[]`, so a `.config.ts` cannot name
    /// a rule by string. Converting to JSON is the only route, and that was the sentence being
    /// cut.
    ///
    /// The specifier is repeated here even though the framing already carries it, because this
    /// is a public error type and a sibling variant read alone names its subject. It costs the
    /// budget twice over, which is why the rest is as short as it is.
    ///
    /// `the_refusal_survives_quickjs_beside_a_long_path` is what holds the arithmetic. See
    /// `AGENTS.md`.
    #[error("`lanekeep/{name}` is a rule component; name it in a `lanekeep.json`")]
    NotAModule {
        /// The built-in's name, without the `lanekeep/` prefix.
        name: String,
    },
}

/// Where rule modules live, and what may be imported.
#[derive(Debug, Clone)]
pub struct RuleRoot {
    root: PathBuf,
    builtins: BuiltinSource,
    builtin_components: BuiltinComponent,
    builtin_component_maps: BuiltinComponentMap,
}

impl RuleRoot {
    /// Anchor resolution at a directory.
    ///
    /// # Errors
    ///
    /// Fails if the directory does not exist or cannot be canonicalized.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ResolveError> {
        let root = root.as_ref();
        let canonical = root.canonicalize().map_err(|e| ResolveError::Unreadable {
            path: root.display().to_string(),
            detail: e.to_string(),
        })?;
        Ok(Self {
            root: canonical,
            builtins: no_builtins,
            builtin_components: no_builtin_components,
            builtin_component_maps: no_builtin_component_maps,
        })
    }

    /// Serve built-in rules from embedded sources.
    ///
    /// Built-ins resolve before anything on disk, so a project file cannot shadow one —
    /// a rule whose behavior depended on whether a same-named file happened to exist
    /// would be impossible to reason about.
    #[must_use]
    pub const fn with_builtins(mut self, builtins: BuiltinSource) -> Self {
        self.builtins = builtins;
        self
    }

    /// Serve built-in rules that ship as components from embedded bytes.
    ///
    /// Beside [`RuleRoot::with_builtins`] rather than replacing it: a built-in is one or the
    /// other, and which one it is is not something a config writes or a user chooses. Both
    /// resolve under the same `lanekeep/` prefix and both resolve before the filesystem.
    #[must_use]
    pub const fn with_builtin_components(mut self, components: BuiltinComponent) -> Self {
        self.builtin_components = components;
        self
    }

    /// Serve the source maps of the built-ins that ship as components.
    ///
    /// Separate from [`RuleRoot::with_builtin_components`] on the terms [`BuiltinComponentMap`]
    /// gives: a map answers a diagnostics question, most components have none, and a caller that
    /// wires one hook and not the other loses a stack rather than a rule.
    #[must_use]
    pub const fn with_builtin_component_maps(mut self, maps: BuiltinComponentMap) -> Self {
        self.builtin_component_maps = maps;
        self
    }

    /// The source map of the component behind a built-in's name, or `None`.
    ///
    /// Asked by `lanekeep-config` beside [`RuleRoot::builtin_component`], with the same name.
    #[must_use]
    pub fn builtin_component_map(&self, name: &str) -> Option<&'static [u8]> {
        (self.builtin_component_maps)(name)
    }

    /// The component behind a built-in's name and the index it sits at, or `None`.
    ///
    /// `lanekeep-config` asks this when a config names `lanekeep/<name>`, because a component
    /// is resolved in Rust and never crosses into the sandbox. Nothing in this crate loads it.
    #[must_use]
    pub fn builtin_component(&self, name: &str) -> Option<(&'static [u8], u32)> {
        (self.builtin_components)(name)
    }

    /// The lookup itself, for a caller that classifies many names at once.
    #[must_use]
    pub const fn builtin_components(&self) -> BuiltinComponent {
        self.builtin_components
    }

    /// The canonical root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Resolve a specifier against the module that imported it.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] for a bare specifier, an escape from the root, or a
    /// specifier matching no file.
    pub fn resolve(&self, base: &str, specifier: &str) -> Result<PathBuf, ResolveError> {
        if specifier == HOST_MODULE {
            return Ok(PathBuf::from(HOST_MODULE));
        }

        // Built-ins resolve before the filesystem is consulted at all.
        if let Some(name) = specifier.strip_prefix(BUILTIN_PREFIX) {
            // A built-in that ships as a component is spelled correctly and is still not
            // importable, so it is refused on its own terms.
            //
            // **Asked before the source lookup, and the order is the whole of the guarantee.**
            // A name can be both: the four TypeScript rules compiled into one component keep
            // their sources, because that is what the component was built from and what their
            // tests run through this engine. Asking the source first would answer an `import`
            // with the QuickJS copy while a `lanekeep.json` ran the component — one id, two
            // programs, and nothing in the output to say which one reported. The component is
            // what ships, so the component is the answer, and the other spelling is refused
            // rather than quietly served.
            if (self.builtin_components)(name).is_some() {
                return Err(ResolveError::NotAModule {
                    name: name.to_owned(),
                });
            }
            if (self.builtins)(name).is_some() {
                return Ok(PathBuf::from(specifier));
            }
            return Err(ResolveError::NotFound {
                specifier: specifier.to_owned(),
                tried: "no built-in rule by that name".to_owned(),
            });
        }

        // The entry module arrives as an already-resolved absolute path, because that is
        // what the caller hands the engine to import. Accepting one is therefore necessary,
        // but only for the entry: an empty base means nothing imported this.
        //
        // A rule writing `import '/etc/passwd'` always has a base — the importing module's
        // own path — so it falls through to the bare-specifier rejection below rather than
        // through this door. Containment is still checked either way.
        if Path::new(specifier).is_absolute() {
            if !base.is_empty() {
                return Err(ResolveError::BareSpecifier {
                    specifier: specifier.to_owned(),
                });
            }
            return self.resolve_within(specifier, &normalize(Path::new(specifier)));
        }

        if !specifier.starts_with('.') {
            return Err(ResolveError::BareSpecifier {
                specifier: specifier.to_owned(),
            });
        }

        let base_dir = if base == HOST_MODULE || base.is_empty() {
            self.root.clone()
        } else {
            Path::new(base)
                .parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf)
        };

        self.resolve_within(specifier, &normalize(&base_dir.join(specifier)))
    }

    /// Confine an already-joined path to the root, and hand back its canonical form.
    ///
    /// **The containment rules, in one place, for callers that resolved a path some other
    /// way.** [`RuleRoot::resolve`] uses it for the candidate it found; `lanekeep-config` uses
    /// it for a `.wasm` rule reference, which is joined against the root by
    /// `json::classify` and never goes near module resolution. Two sets of confinement rules
    /// would be two things to keep right, and the second one is always the one that is wrong:
    /// a lexical check alone looks complete and does not see a symlink.
    ///
    /// Both checks, in this order, and the order is the point. The lexical one fires whatever
    /// is on disk, so `../../secrets` is refused identically whether or not it is there — an
    /// error that depended on that would tell a reader something about the filesystem instead
    /// of about their config. The canonical one is what sees through a symlink, and it can
    /// only be made after the file is known to exist.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::EscapesRoot`] when the path is outside the root either
    /// lexically or after canonicalization, and [`ResolveError::Unreadable`] when it cannot be
    /// canonicalized — which for a path that is simply not there is what "not found" looks
    /// like at this level.
    pub fn confine(&self, specifier: &str, joined: &Path) -> Result<PathBuf, ResolveError> {
        if !joined.starts_with(&self.root) {
            return Err(ResolveError::EscapesRoot {
                specifier: specifier.to_owned(),
            });
        }

        // Canonicalize the file that was actually found. This is the check that holds against
        // symlinks — the lexical test above cannot see through one.
        let canonical = joined
            .canonicalize()
            .map_err(|e| ResolveError::Unreadable {
                path: joined.display().to_string(),
                detail: e.to_string(),
            })?;
        if !canonical.starts_with(&self.root) {
            return Err(ResolveError::EscapesRoot {
                specifier: specifier.to_owned(),
            });
        }
        Ok(canonical)
    }

    /// Find a file for an already-joined path, enforcing containment.
    fn resolve_within(&self, specifier: &str, joined: &Path) -> Result<PathBuf, ResolveError> {
        // Ahead of the candidate loop as well as inside [`RuleRoot::confine`], because a
        // traversal that matches no file at all must still be reported as an escape rather
        // than as "nothing found".
        if !joined.starts_with(&self.root) {
            return Err(ResolveError::EscapesRoot {
                specifier: specifier.to_owned(),
            });
        }

        let mut tried = Vec::new();
        for candidate in candidates(joined) {
            tried.push(candidate.display().to_string());
            if !candidate.is_file() {
                continue;
            }
            return self.confine(specifier, &candidate);
        }

        Err(ResolveError::NotFound {
            specifier: specifier.to_owned(),
            tried: tried.join(", "),
        })
    }

    /// Read a resolved module, stripping types when it is TypeScript.
    ///
    /// Containment is re-checked here rather than trusted from [`RuleRoot::resolve`].
    /// Reading is the operation that actually touches a file, so it should be the thing
    /// that enforces the boundary — otherwise the guarantee depends on every caller having
    /// gone through the resolver first, which is exactly the sort of assumption that holds
    /// until someone adds a second caller.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::EscapesRoot`] if the path is outside the root, or
    /// [`ResolveError::Unreadable`] if the file cannot be read or stripping rejects it.
    pub fn read(
        &self,
        path: &Path,
        typescript: &dyn Language,
        javascript: &dyn Language,
    ) -> Result<String, ResolveError> {
        if path == Path::new(HOST_MODULE) {
            return Ok(HOST_MODULE_SOURCE.to_owned());
        }

        if let Some(name) = path.to_str().and_then(|p| p.strip_prefix(BUILTIN_PREFIX))
            && let Some(source) = (self.builtins)(name)
        {
            // Built-ins are TypeScript like any other rule, so they go through the same
            // stripping — including its verification step. A built-in that failed to strip
            // would be a build-time bug in this repository, and should look like one.
            return strip_types(typescript, javascript, source).map_err(|e| {
                ResolveError::Unreadable {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                }
            });
        }

        let canonical = path.canonicalize().map_err(|e| ResolveError::Unreadable {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(ResolveError::EscapesRoot {
                specifier: path.display().to_string(),
            });
        }

        let source = std::fs::read_to_string(path).map_err(|e| ResolveError::Unreadable {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;

        // Plain JavaScript is passed through untouched rather than run through the
        // stripper, which would only be able to fail on it.
        let is_typescript = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "ts" | "tsx" | "mts" | "cts"));
        if !is_typescript {
            return Ok(source);
        }

        strip_types(typescript, javascript, &source).map_err(|e| ResolveError::Unreadable {
            path: path.display().to_string(),
            detail: e.to_string(),
        })
    }
}

/// Candidate files for a specifier, in resolution order.
fn candidates(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // An explicit extension is taken at face value.
    if base.extension().is_some() {
        out.push(base.to_path_buf());
    }

    for extension in EXTENSIONS {
        out.push(base.with_extension(extension));
    }
    for extension in EXTENSIONS {
        out.push(base.join(format!("index.{extension}")));
    }

    out
}

/// Adapts [`RuleRoot`] to the engine's resolver interface.
#[derive(Debug, Clone)]
pub struct RuleResolver {
    root: RuleRoot,
}

impl RuleResolver {
    /// Build a resolver for a rules root.
    #[must_use]
    pub const fn new(root: RuleRoot) -> Self {
        Self { root }
    }
}

impl Resolver for RuleResolver {
    fn resolve(
        &mut self,
        _ctx: &Ctx<'_>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'_>>,
    ) -> rquickjs::Result<String> {
        match self.root.resolve(base, name) {
            Ok(path) => Ok(path.display().to_string()),
            // The engine's error channel carries only a message, so the diagnostic is
            // rendered here rather than lost.
            Err(err) => Err(JsError::new_resolving_message(
                base.to_owned(),
                name.to_owned(),
                err.to_string(),
            )),
        }
    }
}

/// Every module the loader read, with the source it read.
///
/// This is what makes `ruleset_hash` cover the whole import graph rather than only the
/// entry files. A rule that imports a shared helper has to invalidate when that helper
/// changes, and the only component that knows the helper was involved is the loader.
///
/// Ordered, so the hash derived from it does not depend on load order — which varies with
/// import structure and is not something a user changed.
pub type LoadedModules = Rc<RefCell<BTreeMap<PathBuf, String>>>;

/// Adapts [`RuleRoot`] to the engine's loader interface.
///
/// `Debug` is hand-written because `Arc<dyn Language>` is not `Debug`, and requiring it on
/// the trait would burden every language implementation for one impl here.
#[derive(Clone)]
pub struct RuleLoader {
    root: RuleRoot,
    typescript: Arc<dyn Language>,
    javascript: Arc<dyn Language>,
    loaded: LoadedModules,
}

impl std::fmt::Debug for RuleLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleLoader")
            .field("root", &self.root)
            .field("typescript", &self.typescript.id())
            .field("javascript", &self.javascript.id())
            .field("loaded", &self.loaded.borrow().len())
            .finish()
    }
}

impl RuleLoader {
    /// Build a loader for a rules root.
    ///
    /// The languages are supplied rather than assumed so this crate does not have to know
    /// which grammars exist.
    #[must_use]
    pub fn new(
        root: RuleRoot,
        typescript: Arc<dyn Language>,
        javascript: Arc<dyn Language>,
    ) -> Self {
        Self {
            root,
            typescript,
            javascript,
            loaded: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    /// A handle on what this loader has read, for hashing the rule graph.
    #[must_use]
    pub fn loaded(&self) -> LoadedModules {
        Rc::clone(&self.loaded)
    }
}

impl Loader for RuleLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        let source = self
            .root
            .read(
                Path::new(name),
                self.typescript.as_ref(),
                self.javascript.as_ref(),
            )
            .map_err(|err| JsError::new_loading_message(name.to_owned(), err.to_string()))?;

        // Recorded before declaring, so a module that fails to compile still counts as
        // part of the graph. Otherwise fixing the compile error would not invalidate.
        self.loaded
            .borrow_mut()
            .insert(PathBuf::from(name), source.clone());

        Module::declare(ctx.clone(), name, source)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use lanekeep_lang_js::{JavaScript, TypeScript};

    use super::*;

    /// A rules directory laid out for a test, cleaned up on drop.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("lanekeep-loader-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("creates fixture dir");
            for (path, contents) in files {
                let full = dir.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).expect("creates parent");
                }
                fs::write(&full, contents).expect("writes fixture file");
            }
            Self { dir }
        }

        fn root(&self) -> RuleRoot {
            RuleRoot::new(&self.dir).expect("canonicalizes")
        }

        fn entry(&self, name: &str) -> String {
            self.dir
                .join(name)
                .canonicalize()
                .expect("exists")
                .display()
                .to_string()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Stands in for the real built-in table, so these tests do not depend on which rules
    /// happen to ship.
    fn stub_builtins(name: &str) -> Option<&'static str> {
        match name {
            "always" => Some("export default { id: 'lanekeep/always' } satisfies unknown;"),
            // A rule with a source *and* a component: the ordinary shape of a TypeScript rule
            // compiled ahead of time, where the source is what the artifact was built from and
            // is kept for the tests that run it through this engine.
            "compiled-from-source" => {
                Some("export default { id: 'lanekeep/compiled-from-source' } satisfies unknown;")
            }
            _ => None,
        }
    }

    #[test]
    fn resolves_a_built_in_by_specifier() {
        let fixture = Fixture::new("builtin-resolve", &[("a.ts", "export const a = 1;")]);
        let root = fixture.root().with_builtins(stub_builtins);
        assert_eq!(
            root.resolve("", "lanekeep/always").expect("resolves"),
            Path::new("lanekeep/always")
        );
    }

    #[test]
    fn an_unknown_built_in_is_not_found() {
        // Not "bare specifier": the `lanekeep/` prefix says what the author meant, and an
        // error about npm resolution would send them somewhere useless.
        let fixture = Fixture::new("builtin-unknown", &[("a.ts", "export const a = 1;")]);
        let root = fixture.root().with_builtins(stub_builtins);
        let error = root
            .resolve("", "lanekeep/no-such-rule")
            .expect_err("does not resolve");
        assert!(
            matches!(error, ResolveError::NotFound { .. }),
            "expected NotFound, got {error:?}"
        );
        assert!(error.to_string().contains("built-in"), "{error}");
    }

    /// Stands in for the real component table, on the same terms as [`stub_builtins`].
    fn stub_builtin_components(name: &str) -> Option<(&'static [u8], u32)> {
        match name {
            "compiled" => Some((b"\0asm\x01\x00\x00\x00", 0)),
            // Also served by `stub_builtins`, and at a non-zero index so that a caller
            // discarding the index cannot pass by accident.
            "compiled-from-source" => Some((b"\0asm\x01\x00\x00\x00", 3)),
            _ => None,
        }
    }

    #[test]
    fn a_built_in_that_is_a_component_is_refused_as_a_module() {
        // And refused *as itself*. A component-backed built-in is spelled correctly, so the
        // "no built-in rule by that name" message would send its author looking for a
        // misspelling that is not there. The two facts are different and only one is a typo.
        let fixture = Fixture::new("builtin-component", &[("a.ts", "export const a = 1;")]);
        let root = fixture
            .root()
            .with_builtins(stub_builtins)
            .with_builtin_components(stub_builtin_components);

        let error = root
            .resolve("", "lanekeep/compiled")
            .expect_err("a component is not importable");

        assert!(
            matches!(&error, ResolveError::NotAModule { name } if name == "compiled"),
            "expected NotAModule, got {error:?}"
        );
        let rendered = error.to_string();
        assert!(rendered.contains("lanekeep/compiled"), "{rendered}");
        assert!(rendered.contains("component"), "{rendered}");
        assert!(
            rendered.contains("lanekeep.json"),
            "the message has to name the format that can reach it: {rendered}"
        );
    }

    /// The whole message reaches a user whose project is nested as deeply as a real one.
    ///
    /// **The constraint is not "the message is short", and a test asserting that passed against
    /// the bug it was written for.** QuickJS copies a thrown error into a 255-byte buffer, and
    /// rquickjs hands it `Error resolving module '<specifier>' from '<path>': <message>` — so
    /// what has to hold is `35 + specifier + path + message <= 255`, and the *project's path* is
    /// two of those four terms' worth of budget that this crate does not control. A previous
    /// version of this assertion pinned the first line at 80 bytes, which is a quantity nothing
    /// depends on: the message it was guarding lost its remedy at any config path beyond 47
    /// characters, and the assertion was green throughout.
    ///
    /// **The rule's name is spent twice** — once in rquickjs's framing, once inside the message
    /// — so every character of it costs two of the path budget. A name of 21 characters clears a
    /// 108-byte path; at 15 it cleared 120, and `no-mutable-default-argument`, at 27, would
    /// clear only 96. So this is stated as a *name-length* budget rather than measured against
    /// whichever names happen to ship, which this crate cannot see anyway: it sits below
    /// `lanekeep-rules` deliberately.
    ///
    /// `PATH` moved with `MAX_COMPONENT_NAME` rather than staying put, and that is the point of
    /// the pair: the constraint is one inequality with two knobs, so raising the name budget
    /// without lowering the path budget would be asserting something false. See
    /// [`MAX_COMPONENT_NAME`] for why 108 was judged enough.
    ///
    /// `lanekeep-rules`' `every_component_name_fits_the_refusal_message` is the other half, and
    /// it is what makes this test's premise true rather than assumed. Neither is any use alone:
    /// this one would pass while a longer name silently ate a user's remedy, and that one would
    /// be enforcing a number with no derivation behind it.
    #[test]
    fn the_refusal_survives_quickjs_beside_a_long_path() {
        /// What QuickJS will keep, terminator excluded.
        const BUDGET: usize = 255;
        /// A project path this message must not be truncated beside. Roughly
        /// `/Users/<name>/work/<org>-monorepo/apps/<app>/packages/<pkg>/lanekeep.config.ts`.
        const PATH: usize = 108;

        // Not a literal: this is rquickjs's framing with both holes empty, so the constant
        // cannot drift from the string it is measuring.
        let framing = "Error resolving module '' from '': ".len();

        let name = "x".repeat(MAX_COMPONENT_NAME);
        let specifier = format!("lanekeep/{name}").len();
        let message = ResolveError::NotAModule { name }.to_string();

        let total = framing + specifier + PATH + message.len();
        assert!(
            total <= BUDGET,
            "QuickJS keeps {BUDGET} bytes and this needs {total} beside a {PATH}-byte path \
             ({} of them the message): the remedy is what gets cut\n  {message}",
            message.len(),
        );
    }

    #[test]
    fn a_component_wins_over_a_source_of_the_same_name() {
        // The one that decides what `lanekeep/<name>` means when both lookups answer, which is
        // the ordinary shape of a rule authored in TypeScript and compiled ahead of time.
        //
        // **The failure this is written against is silent.** With the lookups asked the other
        // way round the import succeeds, the sandbox evaluates the author's source, and the
        // run reports — correctly, plausibly, and from a different program than the one a
        // `lanekeep.json` would have run for the same name. Nothing in the output distinguishes
        // them, so the only place this can be caught is here.
        let fixture = Fixture::new("builtin-both", &[("a.ts", "export const a = 1;")]);
        let root = fixture
            .root()
            .with_builtins(stub_builtins)
            .with_builtin_components(stub_builtin_components);

        let error = root
            .resolve("", "lanekeep/compiled-from-source")
            .expect_err("the component is what ships, so the import is refused");
        assert!(
            matches!(&error, ResolveError::NotAModule { name } if name == "compiled-from-source"),
            "expected NotAModule, got {error:?}"
        );

        // And the source is still there to be read by whatever built the component, which is
        // why the two lookups can disagree at all.
        assert!(
            stub_builtins("compiled-from-source").is_some(),
            "the fixture must have both, or this test asserts nothing"
        );
    }

    #[test]
    fn an_unknown_name_is_still_not_found_when_components_ship() {
        // The component lookup must not turn every miss into "it is a component".
        let fixture = Fixture::new("builtin-component-miss", &[("a.ts", "export const a = 1;")]);
        let root = fixture
            .root()
            .with_builtins(stub_builtins)
            .with_builtin_components(stub_builtin_components);

        let error = root
            .resolve("", "lanekeep/no-such-rule")
            .expect_err("does not resolve");

        assert!(
            matches!(error, ResolveError::NotFound { .. }),
            "expected NotFound, got {error:?}"
        );
    }

    #[test]
    fn a_component_is_reachable_by_name_without_being_importable() {
        let fixture = Fixture::new(
            "builtin-component-bytes",
            &[("a.ts", "export const a = 1;")],
        );
        let root = fixture
            .root()
            .with_builtin_components(stub_builtin_components);

        assert_eq!(
            root.builtin_component("compiled"),
            Some((b"\0asm\x01\x00\x00\x00".as_slice(), 0))
        );
        // The index travels with the bytes, so a rule of a shared component is reachable as
        // itself rather than as whichever rule that artifact enumerates first.
        assert_eq!(
            root.builtin_component("compiled-from-source"),
            Some((b"\0asm\x01\x00\x00\x00".as_slice(), 3))
        );
        assert_eq!(root.builtin_component("always"), None);
    }

    #[test]
    fn a_file_cannot_shadow_a_built_in() {
        // A rules directory containing `lanekeep/always.ts` must not change what the
        // specifier means. A rule whose behavior depended on whether a same-named file
        // happened to exist would be unreasonable to debug.
        let fixture = Fixture::new(
            "builtin-shadow",
            &[("lanekeep/always.ts", "export default 'the wrong one';")],
        );
        let root = fixture.root().with_builtins(stub_builtins);
        let resolved = root.resolve("", "lanekeep/always").expect("resolves");
        assert_eq!(resolved, Path::new("lanekeep/always"));

        let source = root
            .read(&resolved, &TypeScript, &JavaScript)
            .expect("reads");
        assert!(
            !source.contains("the wrong one"),
            "a project file shadowed a built-in: {source}"
        );
    }

    #[test]
    fn a_built_in_is_stripped_of_its_types() {
        let fixture = Fixture::new("builtin-strip", &[("a.ts", "export const a = 1;")]);
        let root = fixture.root().with_builtins(stub_builtins);
        let source = root
            .read(Path::new("lanekeep/always"), &TypeScript, &JavaScript)
            .expect("reads");
        assert!(
            !source.contains("satisfies"),
            "type syntax survived stripping: {source}"
        );
    }

    #[test]
    fn built_ins_are_absent_unless_provided() {
        // The default. A crate embedding `lanekeep-js` without the rules crate resolves
        // project modules only, rather than silently resolving names to nothing.
        let fixture = Fixture::new("builtin-default", &[("a.ts", "export const a = 1;")]);
        let root = fixture.root();
        assert!(root.resolve("", "lanekeep/always").is_err());
    }

    #[test]
    fn resolves_the_host_module() {
        let fixture = Fixture::new("host", &[("a.ts", "export const a = 1;")]);
        let root = fixture.root();
        assert_eq!(
            root.resolve("", HOST_MODULE).expect("resolves"),
            Path::new(HOST_MODULE)
        );
    }

    #[test]
    fn the_host_module_exports_the_authoring_helpers() {
        let fixture = Fixture::new("host-src", &[]);
        let source = fixture
            .root()
            .read(Path::new(HOST_MODULE), &TypeScript, &JavaScript)
            .expect("reads");
        assert!(source.contains("defineRule"), "{source}");
        assert!(source.contains("defineConfig"), "{source}");
    }

    #[test]
    fn resolves_a_relative_import() {
        let fixture = Fixture::new(
            "relative",
            &[
                ("main.ts", "import './helper';"),
                ("helper.ts", "export const h = 1;"),
            ],
        );
        let root = fixture.root();

        let resolved = root
            .resolve(&fixture.entry("main.ts"), "./helper")
            .expect("resolves");
        assert!(resolved.ends_with("helper.ts"), "{resolved:?}");
    }

    #[test]
    fn tries_extensions_in_order() {
        // A `.ts` file wins over a `.js` file of the same name, because a rule directory
        // containing both is almost always a stale build artifact next to its source.
        let fixture = Fixture::new(
            "extensions",
            &[
                ("main.ts", ""),
                ("dup.ts", "export const from = 'ts';"),
                ("dup.js", "export const from = 'js';"),
            ],
        );
        let resolved = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "./dup")
            .expect("resolves");
        assert!(
            resolved.ends_with("dup.ts"),
            "expected the TypeScript file: {resolved:?}"
        );
    }

    #[test]
    fn resolves_a_directory_index() {
        let fixture = Fixture::new(
            "index",
            &[("main.ts", ""), ("rules/index.ts", "export const r = 1;")],
        );
        let resolved = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "./rules")
            .expect("resolves");
        assert!(resolved.ends_with("index.ts"), "{resolved:?}");
    }

    #[test]
    fn a_file_beats_a_same_named_directory_index() {
        // `candidates` puts every extension ahead of every `index.<extension>`, and until this
        // test nothing held it there — `resolves_a_directory_index` only proves an index wins
        // when no file competes, which it would do under either order.
        //
        // The rule is the one every bundler and TypeScript itself follows, so getting it wrong
        // would not look like a bug from inside a rule: `./rules` would simply mean the other
        // file, and both readings are individually plausible. It is also enforced twice now —
        // `packages/lanekeep/runtime/resolve.js` resolves a rule's imports at build time — so
        // leaving it unpinned here would let the port diverge from the specification with
        // nothing to notice, which is most of what makes this file the specification.
        let fixture = Fixture::new(
            "file-over-index",
            &[
                ("main.ts", ""),
                ("rules.ts", "export const from = 'file';"),
                ("rules/index.ts", "export const from = 'index';"),
            ],
        );
        let resolved = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "./rules")
            .expect("resolves");
        assert!(
            resolved.ends_with("rules.ts"),
            "expected the file, not the directory: {resolved:?}"
        );
    }

    #[test]
    fn resolves_an_explicit_extension() {
        let fixture = Fixture::new(
            "explicit",
            &[("main.ts", ""), ("helper.ts", "export const h = 1;")],
        );
        let resolved = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "./helper.ts")
            .expect("resolves");
        assert!(resolved.ends_with("helper.ts"), "{resolved:?}");
    }

    // --- what must not resolve --------------------------------------------------------

    #[test]
    fn rejects_bare_specifiers() {
        let fixture = Fixture::new("bare", &[("main.ts", "")]);
        let root = fixture.root();

        for specifier in [
            "lodash",
            "react",
            "node:fs",
            "fs",
            "@scope/pkg",
            "typescript",
        ] {
            let err = root
                .resolve(&fixture.entry("main.ts"), specifier)
                .expect_err("bare specifiers must not resolve");
            assert!(
                matches!(err, ResolveError::BareSpecifier { .. }),
                "{specifier} gave {err:?}"
            );
        }
    }

    #[test]
    fn a_bare_specifier_explains_why() {
        let fixture = Fixture::new("bare-msg", &[("main.ts", "")]);
        let err = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "lodash")
            .expect_err("bare specifiers do not resolve");

        assert!(matches!(err, ResolveError::BareSpecifier { .. }), "{err:?}");
        let rendered = err.to_string();
        assert!(rendered.contains("no package resolution"), "{rendered}");
        assert!(rendered.contains("lanekeep"), "{rendered}");
    }

    #[test]
    fn rejects_traversal_out_of_the_root() {
        let fixture = Fixture::new("traversal", &[("main.ts", "")]);
        let root = fixture.root();
        let base = fixture.entry("main.ts");

        for specifier in ["../outside", "../../etc/passwd", "./../../secrets", "../"] {
            let err = root
                .resolve(&base, specifier)
                .expect_err("traversal must not resolve");
            assert!(
                matches!(err, ResolveError::EscapesRoot { .. }),
                "{specifier} gave {err:?}"
            );
        }
    }

    #[test]
    fn traversal_is_rejected_even_when_the_target_exists() {
        // The lexical check has to fire regardless of what is on disk, or the error a
        // reader sees depends on whether the file they tried to reach happened to be there.
        let fixture = Fixture::new(
            "traversal-real",
            &[("nested/main.ts", ""), ("secret.ts", "export const s = 1;")],
        );
        let root = RuleRoot::new(fixture.dir.join("nested")).expect("canonicalizes");

        let err = root
            .resolve(&fixture.entry("nested/main.ts"), "../secret")
            .expect_err("must not escape");
        assert!(matches!(err, ResolveError::EscapesRoot { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_pointing_outside_the_root() {
        // The case a lexical check cannot see. `./link` looks entirely innocent; only
        // canonicalizing the file that was found reveals where it goes.
        let fixture = Fixture::new(
            "symlink",
            &[
                ("nested/main.ts", ""),
                ("outside.ts", "export const o = 1;"),
            ],
        );
        let root_dir = fixture.dir.join("nested");
        let link = root_dir.join("link.ts");
        std::os::unix::fs::symlink(fixture.dir.join("outside.ts"), &link).expect("creates symlink");

        let root = RuleRoot::new(&root_dir).expect("canonicalizes");
        let err = root
            .resolve(&fixture.entry("nested/main.ts"), "./link")
            .expect_err("a symlink out of the root must be rejected");
        assert!(matches!(err, ResolveError::EscapesRoot { .. }), "{err:?}");
    }

    #[test]
    fn a_rule_may_not_import_an_absolute_path() {
        // The entry module legitimately arrives as an absolute path, so the resolver has
        // to accept one. This checks that door is only open for the entry — a rule with a
        // base of its own is refused.
        //
        // The absolute path is built from `temp_dir` rather than written literally,
        // because `Path::is_absolute` is platform-specific: `/etc/passwd` is absolute on
        // Unix and merely rooted on Windows, while `C:\...` is the reverse. A literal
        // would take a different branch on each platform and assert a different error.
        let fixture = Fixture::new("absolute", &[("main.ts", "")]);
        let root = fixture.root();
        let base = fixture.entry("main.ts");

        let outside = std::env::temp_dir().join("lanekeep-absolute-probe.ts");
        let outside = outside.display().to_string();

        // Written by a rule: refused, whichever way the platform classifies it.
        for specifier in [outside.as_str(), "/etc/passwd", "C:\\Windows\\System32\\x"] {
            assert!(
                root.resolve(&base, specifier).is_err(),
                "a rule must not import `{specifier}`"
            );
        }

        // As an entry point: still refused, because it is outside the root.
        let err = root
            .resolve("", &outside)
            .expect_err("an entry outside the root must be refused");
        assert!(matches!(err, ResolveError::EscapesRoot { .. }), "{err:?}");
    }

    #[test]
    fn reports_what_it_tried_when_nothing_matches() {
        let fixture = Fixture::new("missing", &[("main.ts", "")]);
        let err = fixture
            .root()
            .resolve(&fixture.entry("main.ts"), "./nope")
            .expect_err("nothing to find");

        match err {
            ResolveError::NotFound { tried, .. } => {
                assert!(tried.contains("nope.ts"), "should list candidates: {tried}");
                assert!(
                    tried.contains("index.ts"),
                    "should list index candidates: {tried}"
                );
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    // --- reading ------------------------------------------------------------------------

    #[test]
    fn strips_types_when_reading_typescript() {
        let fixture = Fixture::new(
            "read-ts",
            &[("a.ts", "export const a: number = 1;\ninterface B {}\n")],
        );
        let root = fixture.root();
        let path = root.resolve("", "./a").expect("resolves");
        let source = root.read(&path, &TypeScript, &JavaScript).expect("reads");

        assert!(!source.contains(": number"), "{source}");
        assert!(!source.contains("interface"), "{source}");
        assert!(source.contains("export const a"), "{source}");
    }

    #[test]
    fn reading_refuses_a_path_outside_the_root_even_if_resolution_was_skipped() {
        // Defense in depth. `resolve` already enforces this, but a future caller that
        // builds a path some other way must not be able to read past the boundary.
        let fixture = Fixture::new(
            "read-escape",
            &[
                ("nested/main.ts", ""),
                ("outside.ts", "export const o = 1;"),
            ],
        );
        let root = RuleRoot::new(fixture.dir.join("nested")).expect("canonicalizes");

        let err = root
            .read(&fixture.dir.join("outside.ts"), &TypeScript, &JavaScript)
            .expect_err("reading outside the root must be refused");
        assert!(matches!(err, ResolveError::EscapesRoot { .. }), "{err:?}");
    }

    #[test]
    fn passes_javascript_through_untouched() {
        let contents = "export const a = 1;\n";
        let fixture = Fixture::new("read-js", &[("a.js", contents)]);
        let root = fixture.root();
        let path = root.resolve("", "./a.js").expect("resolves");
        assert_eq!(
            root.read(&path, &TypeScript, &JavaScript).expect("reads"),
            contents
        );
    }

    // --- end to end, through the engine ---------------------------------------------
    //
    // Everything above tests the resolution logic directly. These go through the engine's
    // Resolver and Loader adapters, which is the part that is actually wired up at runtime
    // and could be correct in isolation while being connected wrongly.

    fn sandbox_for(fixture: &Fixture) -> crate::Sandbox {
        crate::Sandbox::with_modules(
            crate::Limits::default(),
            crate::RunClock::start(std::time::Duration::from_secs(30)),
            fixture.root(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .expect("sandbox builds")
    }

    #[test]
    fn loads_a_rule_module_that_imports_the_host_module() {
        let fixture = Fixture::new(
            "e2e-host",
            &[(
                "rule.ts",
                "import { defineRule } from 'lanekeep';\n\
                 export default defineRule({ id: 'local/example' });\n",
            )],
        );
        let sandbox = sandbox_for(&fixture);
        let path = fixture.root().resolve("", "./rule").expect("resolves");

        let module: std::collections::HashMap<String, String> =
            sandbox.import_default(&path).expect("module evaluates");
        assert_eq!(module.get("id").map(String::as_str), Some("local/example"));
    }

    #[test]
    fn loads_a_module_that_imports_a_sibling_and_strips_its_types() {
        let fixture = Fixture::new(
            "e2e-sibling",
            &[
                (
                    "rule.ts",
                    "import { defineRule } from 'lanekeep';\n\
                     import { NAME } from './shared';\n\
                     export default defineRule({ id: NAME });\n",
                ),
                (
                    "shared.ts",
                    "interface Unused { a: number }\n\
                     export const NAME: string = 'local/from-sibling';\n",
                ),
            ],
        );
        let sandbox = sandbox_for(&fixture);
        let path = fixture.root().resolve("", "./rule").expect("resolves");

        let module: std::collections::HashMap<String, String> =
            sandbox.import_default(&path).expect("module evaluates");
        assert_eq!(
            module.get("id").map(String::as_str),
            Some("local/from-sibling")
        );
    }

    #[test]
    fn a_bare_import_fails_at_load_with_the_explanation() {
        let fixture = Fixture::new(
            "e2e-bare",
            &[(
                "rule.ts",
                "import lodash from 'lodash';\nexport default lodash;\n",
            )],
        );
        let sandbox = sandbox_for(&fixture);
        let path = fixture.root().resolve("", "./rule").expect("resolves");

        let err = sandbox
            .import_default::<std::collections::HashMap<String, String>>(&path)
            .expect_err("lodash cannot resolve");
        let rendered = err.to_string();
        assert!(rendered.contains("lodash"), "{rendered}");
    }

    #[test]
    fn a_traversing_import_fails_at_load() {
        let fixture = Fixture::new(
            "e2e-traversal",
            &[
                (
                    "nested/rule.ts",
                    "import x from '../outside';\nexport default x;\n",
                ),
                ("outside.ts", "export default 1;\n"),
            ],
        );
        let root = RuleRoot::new(fixture.dir.join("nested")).expect("canonicalizes");
        let sandbox = crate::Sandbox::with_modules(
            crate::Limits::default(),
            crate::RunClock::start(std::time::Duration::from_secs(30)),
            root.clone(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .expect("sandbox builds");

        let path = root.resolve("", "./rule").expect("resolves");
        assert!(
            sandbox
                .import_default::<std::collections::HashMap<String, String>>(&path)
                .is_err(),
            "an import escaping the root must not load"
        );
    }

    #[test]
    fn a_module_that_fails_to_strip_reports_the_reason() {
        let fixture = Fixture::new("read-bad", &[("a.ts", "enum E { A }\n")]);
        let root = fixture.root();
        let path = root.resolve("", "./a").expect("resolves");
        let err = root
            .read(&path, &TypeScript, &JavaScript)
            .expect_err("enums are rejected");

        let rendered = err.to_string();
        assert!(
            rendered.contains("enum"),
            "should name the construct: {rendered}"
        );
    }
}
