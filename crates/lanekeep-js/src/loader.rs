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

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

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
}

/// Resolve a specifier lexically, without consulting the filesystem for `..`.
///
/// `Path::join` followed by `canonicalize` would also work, but only for paths that exist —
/// and a traversal attempt should be rejected with a message about escaping the root
/// whether or not the target happens to be there.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Popping past the start is what an escape looks like lexically. Keep
                    // the marker so the caller's containment check fails.
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where rule modules live, and what may be imported.
#[derive(Debug, Clone)]
pub struct RuleRoot {
    root: PathBuf,
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
        Ok(Self { root: canonical })
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

    /// Find a file for an already-joined path, enforcing containment.
    fn resolve_within(&self, specifier: &str, joined: &Path) -> Result<PathBuf, ResolveError> {
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

            // Canonicalize the file that was actually found. This is the check that holds
            // against symlinks — the lexical test above cannot see through one.
            let canonical = candidate
                .canonicalize()
                .map_err(|e| ResolveError::Unreadable {
                    path: candidate.display().to_string(),
                    detail: e.to_string(),
                })?;
            if !canonical.starts_with(&self.root) {
                return Err(ResolveError::EscapesRoot {
                    specifier: specifier.to_owned(),
                });
            }
            return Ok(canonical);
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

/// Adapts [`RuleRoot`] to the engine's loader interface.
///
/// `Debug` is hand-written because `Arc<dyn Language>` is not `Debug`, and requiring it on
/// the trait would burden every language implementation for one impl here.
#[derive(Clone)]
pub struct RuleLoader {
    root: RuleRoot,
    typescript: Arc<dyn Language>,
    javascript: Arc<dyn Language>,
}

impl std::fmt::Debug for RuleLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleLoader")
            .field("root", &self.root)
            .field("typescript", &self.typescript.id())
            .field("javascript", &self.javascript.id())
            .finish()
    }
}

impl RuleLoader {
    /// Build a loader for a rules root.
    ///
    /// The languages are supplied rather than assumed so this crate does not have to know
    /// which grammars exist.
    #[must_use]
    pub const fn new(
        root: RuleRoot,
        typescript: Arc<dyn Language>,
        javascript: Arc<dyn Language>,
    ) -> Self {
        Self {
            root,
            typescript,
            javascript,
        }
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
