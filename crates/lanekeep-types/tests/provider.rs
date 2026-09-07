//! The provider seam, the resolver, the declaration walk and the three APIs.
//!
//! Integration rather than unit tests for the reason `tests/oracle.rs` gives: the whole job
//! is reading a grammar's output and a real filesystem, and a hand-built tree or a stubbed
//! reader would be a second opinion about those rather than a test of the first.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the lint's grant covers `#[test]` functions and `#[cfg(test)]` modules, and a \
              helper in an integration-test crate is neither — see AGENTS.md"
)]

use std::path::PathBuf;

use lanekeep_core::{FileAccess, FilePath};
use lanekeep_lang_js::TypeScript;
use lanekeep_types::{Query, TypeProvider};

/// A throwaway project on disk, named after the test that built it.
///
/// **Named after the test, never derived from its contents.** `lanekeep-config`'s own
/// `json.rs` helper keyed a fixture directory on the config's length, then on a `blake3` of
/// it, and both raced: two tests can legitimately write identical bytes, and
/// `std::fs::write` truncates before it writes, so a sibling thread reads an empty file.
/// AGENTS.md records five failures in eighty runs. A name is the one key nothing derives.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(test: &str, files: &[(&str, &str)]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("lanekeep-provider-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");

        let project = Self { dir };
        for (path, contents) in files {
            project.write(path, contents);
        }
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates the parent directory");
        }
        std::fs::write(full, contents).expect("writes the fixture file");
    }

    fn files(&self) -> FileAccess {
        FileAccess::new(&self.dir)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Parse `source` with the TypeScript grammar, for building a `Query` by hand.
fn parse(source: &str) -> tree_sitter::Tree {
    use lanekeep_lang::Language;
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

/// A `Query` is one call's worth of context and borrows nothing longer than the call.
///
/// The whole reason the seam is a trait rather than `TypeScriptOracle` itself: that type
/// borrows one tree for its life, which a whole-program provider cannot offer. This asserts
/// the shape a provider is handed, before any provider exists to be handed it.
#[test]
fn a_query_carries_one_calls_context() {
    let project = Project::new("query-shape", &[("src/a.ts", "const x = 1;\n")]);
    let files = project.files();
    let source = "const x = 1;\n";
    let tree = parse(source);
    let file = FilePath::new("src/a.ts");

    let query = Query {
        file: &file,
        tree: &tree,
        source,
        node: last_of(&tree, "number"),
        files: &files,
    };

    assert_eq!(query.file.as_str(), "src/a.ts");
    assert_eq!(query.node.kind(), "number");
    assert_eq!(query.source, source);
    // `Copy`, so a provider arm can hand the same question to a helper without a clone.
    let again: Query<'_> = query;
    assert_eq!(again.node.kind(), "number");
}

/// `TypeProvider` is object-safe, which is the property the engine rests on.
///
/// A `const` block rather than a runtime assertion, on the reasoning `FileAccess`'s
/// `assert_shareable` uses: this is a property of the trait, and a violation should stop the
/// build at the method that caused it rather than surface as an unsatisfied bound in the
/// engine. `Send + Sync` too — rayon moves the `Arc` between workers.
#[test]
fn the_provider_trait_is_shareable_and_object_safe() {
    const fn assert_shareable<T: Send + Sync + ?Sized>() {}
    assert_shareable::<dyn TypeProvider>();
}

/// The provider answers what the oracle answered, through the new door.
///
/// The whole of Group A is a refactor, so the assertion is equality with the behavior
/// `tests/oracle.rs` already pins rather than anything new.
#[test]
fn the_builtin_provider_answers_a_primitive_annotation() {
    let project = Project::new("builtin-primitive", &[]);
    let files = project.files();
    let source = "function credit(amount: number) { return amount; }\n";
    let tree = parse(source);
    let file = FilePath::new("src/a.ts");
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");

    let node = last_of(&tree, "identifier");
    let query = Query {
        file: &file,
        tree: &tree,
        source,
        node,
        files: &files,
    };
    assert_eq!(
        provider.type_of(query),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        ))
    );
}

/// A grammar that does not speak TypeScript yields no provider, exactly as it yielded no
/// `TypeScriptSupport`. This is the engine's per-language gate and it must not soften.
#[test]
fn a_grammar_that_does_not_speak_typescript_yields_no_provider() {
    assert!(lanekeep_types::BuiltinProvider::probe(&lanekeep_lang_python::Python).is_none());
}

/// The identity moves with this crate's source, because it is a cache-key input.
///
/// Only what can be asserted is asserted — that it is populated, stable within a process and
/// carries the oracle's own digest. The property that matters, that it changes when the
/// oracle changes, is structural: `build.rs` hashes all of `src/` under `rerun-if-changed`,
/// and a test able to fail would have to edit its own source.
#[test]
fn the_builtin_providers_identity_carries_the_oracles() {
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let identity = provider.identity();
    assert!(identity.starts_with(b"builtin:"), "{identity:?}");
    assert!(identity.ends_with(&lanekeep_types::oracle_identity()));
    assert_eq!(identity, provider.identity());
}

use lanekeep_types::resolve_specifier;

#[test]
fn a_relative_specifier_finds_a_sibling_source_file() {
    let project = Project::new(
        "relative-source",
        &[
            ("src/a.ts", ""),
            ("src/money.ts", "export const rate = 1;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money"),
        Some(FilePath::new("src/money.ts"))
    );
}

#[test]
fn a_relative_specifier_falls_back_to_a_declaration_file() {
    let project = Project::new(
        "relative-declaration",
        &[
            ("src/a.ts", ""),
            ("src/money.d.ts", "export declare const rate: number;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money"),
        Some(FilePath::new("src/money.d.ts"))
    );
}

#[test]
fn a_relative_specifier_falls_back_to_a_directory_index() {
    let project = Project::new(
        "relative-index",
        &[
            ("src/a.ts", ""),
            ("src/money/index.ts", "export const rate = 1;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money"),
        Some(FilePath::new("src/money/index.ts"))
    );
}

/// The source file wins over the declaration file beside it.
///
/// Not a preference: a `.d.ts` generated from a `.ts` in the same tree is a build artifact
/// that can be stale, and the source is what the program means. Asserted head-on because
/// both files exist and either order of probes finds *something*, so a wrong order is
/// invisible in every test that writes only one of them.
#[test]
fn a_source_file_beats_the_declaration_file_beside_it() {
    let project = Project::new(
        "relative-order",
        &[
            ("src/a.ts", ""),
            ("src/money.ts", "export const rate = 1;\n"),
            ("src/money.d.ts", "export declare const rate: string;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money"),
        Some(FilePath::new("src/money.ts"))
    );
}

/// TypeScript's ESM spelling: the import names the emitted `.js`, the declaration is `.ts`.
#[test]
fn a_specifier_naming_the_emitted_javascript_resolves_to_its_source() {
    let project = Project::new(
        "relative-js-suffix",
        &[
            ("src/a.ts", ""),
            ("src/money.ts", "export const rate = 1;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money.js"),
        Some(FilePath::new("src/money.ts"))
    );
}

/// A `.tsx` sibling is deliberately unresolvable, and the reason is not a missing entry.
///
/// This provider parses every declaration file with one grammar. Parsing a `.tsx` with the
/// TypeScript grammar turns each JSX element into an `ERROR` node **silently** — AGENTS.md's
/// "the grammar that parses a file is chosen by the file", which produced 2218 false
/// positives in one rule the last time it was got wrong. A confidently wrong parse is worse
/// than the `undefined` a miss produces, and the importing file is then honestly incomplete.
#[test]
fn a_tsx_sibling_is_not_resolved_by_the_builtin_provider() {
    let project = Project::new(
        "relative-tsx",
        &[
            ("src/a.ts", ""),
            ("src/Button.tsx", "export const B = () => <b/>;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./Button"),
        None
    );
}

/// A traversal that would leave the root is refused before the filesystem is touched, and
/// records nothing — the same posture `FileAccess` takes on `../outside.json`.
#[test]
fn a_relative_specifier_that_escapes_the_root_resolves_to_nothing() {
    let project = Project::new("relative-escape", &[("a.ts", "")]);
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("a.ts"), "../../secrets"),
        None
    );
    assert!(
        files.dependencies().is_empty(),
        "nothing outside the root is probed"
    );
}

/// Every probe is recorded, misses included, in a fixed order.
///
/// This is the cache half of the design rather than a detail: a `./money.ts` that does not
/// exist today and appears tomorrow must invalidate the importer, and the only thing that can
/// make that happen is a recorded read with a null hash. The exact list is asserted because
/// "some reads happened" would pass against a probe order that changed between runs.
#[test]
fn every_relative_probe_is_recorded_in_a_fixed_order() {
    let project = Project::new("relative-probes", &[("src/a.ts", "")]);
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./money"),
        None
    );

    let recorded: Vec<(String, bool)> = files
        .dependencies()
        .into_iter()
        .map(|read| (read.path.as_str().to_owned(), read.hash.is_some()))
        .collect();
    assert_eq!(
        recorded,
        vec![
            ("src/money.cts".to_owned(), false),
            ("src/money.d.ts".to_owned(), false),
            ("src/money.mts".to_owned(), false),
            ("src/money.ts".to_owned(), false),
            ("src/money/index.d.ts".to_owned(), false),
            ("src/money/index.ts".to_owned(), false),
        ],
        "dependencies come back in path order, and every miss is one"
    );
}

/// The worked fixture for every bare case below: one package, one declaration file.
fn package(manifest: &str) -> Vec<(&'static str, String)> {
    vec![
        ("src/a.ts", String::new()),
        ("node_modules/money/package.json", manifest.to_owned()),
        (
            "node_modules/money/build/index.d.ts",
            "export declare const rate: number;\n".to_owned(),
        ),
    ]
}

/// Write an owned fixture list, which `Project::new`'s `&str` pairs cannot hold.
fn project_with(test: &str, files: &[(&'static str, String)]) -> Project {
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, contents)| (*path, contents.as_str()))
        .collect();
    Project::new(test, &borrowed)
}

#[test]
fn a_bare_specifier_resolves_through_the_exports_types_condition() {
    let project = project_with(
        "bare-exports",
        &package(r#"{"exports": {".": {"types": "./build/index.d.ts", "default": "./x.js"}}}"#),
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/build/index.d.ts"))
    );
}

#[test]
fn a_bare_specifier_resolves_through_a_types_field() {
    let project = project_with("bare-types", &package(r#"{"types": "./build/index.d.ts"}"#));
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/build/index.d.ts"))
    );
}

#[test]
fn a_bare_specifier_resolves_through_a_typings_field() {
    let project = project_with(
        "bare-typings",
        &package(r#"{"typings": "./build/index.d.ts"}"#),
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/build/index.d.ts"))
    );
}

#[test]
fn a_package_with_no_manifest_entry_falls_back_to_its_index() {
    let project = Project::new(
        "bare-index",
        &[
            ("src/a.ts", ""),
            ("node_modules/money/package.json", "{}"),
            (
                "node_modules/money/index.d.ts",
                "export declare const rate: number;\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/index.d.ts"))
    );
}

#[test]
fn a_subpath_export_resolves_to_its_own_target() {
    let project = Project::new(
        "bare-subpath",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"exports": {".": {"types": "./build/index.d.ts"},
                    "./decimal": {"types": "./build/decimal.d.ts"}}}"#,
            ),
            (
                "node_modules/money/build/index.d.ts",
                "export declare const rate: number;\n",
            ),
            (
                "node_modules/money/build/decimal.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money/decimal"),
        Some(FilePath::new("node_modules/money/build/decimal.d.ts"))
    );
}

/// A `*` pattern, and the longest literal prefix winning over a broader one.
///
/// Both halves in one test because either alone is satisfied by the wrong implementation:
/// with only the broad pattern, any matcher passes; with only the narrow one, an
/// implementation that takes whichever key it met first passes half the time.
#[test]
fn a_star_pattern_export_substitutes_the_matched_run_and_the_longest_prefix_wins() {
    let project = Project::new(
        "bare-star",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"exports": {"./*": {"types": "./build/*.d.ts"},
                    "./deep/*": {"types": "./build/deep/*.d.ts"}}}"#,
            ),
            (
                "node_modules/money/build/decimal.d.ts",
                "export declare class Decimal {}\n",
            ),
            (
                "node_modules/money/build/deep/nested.d.ts",
                "export declare class Nested {}\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money/decimal"),
        Some(FilePath::new("node_modules/money/build/decimal.d.ts"))
    );
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money/deep/nested"),
        Some(FilePath::new("node_modules/money/build/deep/nested.d.ts"))
    );
}

/// The `@types` fallback, including the scope-flattening spelling.
#[test]
fn a_package_with_no_types_of_its_own_falls_back_to_at_types() {
    let project = Project::new(
        "bare-at-types",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"main": "./index.js"}"#,
            ),
            (
                "node_modules/@types/money/index.d.ts",
                "export declare const rate: number;\n",
            ),
            (
                "node_modules/@types/acme__money/index.d.ts",
                "export declare const other: number;\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/@types/money/index.d.ts"))
    );
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "@acme/money"),
        Some(FilePath::new("node_modules/@types/acme__money/index.d.ts"))
    );
}

/// The walk goes up from the importing file's directory, and stops at the project root.
#[test]
fn the_walk_climbs_from_the_importing_directory_to_the_root() {
    let project = Project::new(
        "bare-walk",
        &[
            ("apps/web/src/deep/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export declare const rate: number;\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("apps/web/src/deep/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/index.d.ts"))
    );
}

/// The nearer `node_modules` wins, which is what makes the climb a climb.
#[test]
fn a_nearer_node_modules_shadows_one_further_up() {
    let project = Project::new(
        "bare-shadow",
        &[
            ("apps/web/a.ts", ""),
            (
                "apps/web/node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "apps/web/node_modules/money/index.d.ts",
                "export declare const near: number;\n",
            ),
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export declare const far: number;\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("apps/web/a.ts"), "money"),
        Some(FilePath::new("apps/web/node_modules/money/index.d.ts"))
    );
}

/// The confinement case, stated as behavior rather than as an error.
///
/// A `node_modules` hoisted above the project root cannot be reached, and the design says so
/// rather than working around it: the walk stops at the root, in-root candidates are probed
/// and recorded as absent, and the answer is nothing. Every recorded path is inside the root,
/// which is the half that would fail if the walk ever formed a `../` candidate.
#[test]
fn a_hoisted_node_modules_is_unresolvable_and_probes_nothing_above_the_root() {
    let outer = std::env::temp_dir().join(format!(
        "lanekeep-provider-hoisted-outer-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&outer);
    std::fs::create_dir_all(outer.join("node_modules/money")).expect("creates the hoisted tree");
    std::fs::write(
        outer.join("node_modules/money/index.d.ts"),
        "export declare const rate: number;\n",
    )
    .expect("writes the hoisted declaration");
    std::fs::create_dir_all(outer.join("app/src")).expect("creates the inner root");
    std::fs::write(outer.join("app/src/a.ts"), "").expect("writes the importer");

    let files = FileAccess::new(&outer.join("app"));
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        None,
        "nothing above the project root is readable, ever"
    );
    for read in files.dependencies() {
        assert!(
            !read.path.as_str().starts_with(".."),
            "probed outside the root: {}",
            read.path
        );
        assert_eq!(read.hash, None, "every in-root candidate is absent");
    }
    let _ = std::fs::remove_dir_all(&outer);
}

/// An earlier `RELATIVE_SUFFIXES` entry wins over a later one when both files exist.
///
/// `every_relative_probe_is_recorded_in_a_fixed_order` cannot pin this: `dependencies()` is
/// sorted by path, so it reports the same set regardless of which suffix the resolver
/// actually picked. This asserts the *answer*, which can only be right if `.ts` was tried
/// before `.mts` — proven by swapping the two entries in `RELATIVE_SUFFIXES` locally and
/// watching this test fail, then restoring the order.
#[test]
fn an_earlier_suffix_beats_a_later_one_when_both_exist() {
    let project = Project::new(
        "relative-order-suffix",
        &[
            ("src/a.ts", ""),
            ("src/x.ts", "export const rate = 1;\n"),
            ("src/x.mts", "export const rate = 2;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./x"),
        Some(FilePath::new("src/x.ts"))
    );
}

/// An extension probe beats the directory-index fallback when both exist.
///
/// Same reasoning as the sibling test above: `dependencies()`'s sorted output cannot tell
/// `src/x.ts` and `src/x/index.ts` apart by order, only the resolved answer can. Proven by
/// swapping `.ts` and `/index.ts` in `RELATIVE_SUFFIXES` locally — this test then fails
/// because the resolver picks the index file — then restoring the order.
#[test]
fn an_extension_probe_beats_the_index_fallback_when_both_exist() {
    let project = Project::new(
        "relative-order-index",
        &[
            ("src/a.ts", ""),
            ("src/x.ts", "export const rate = 1;\n"),
            ("src/x/index.ts", "export const rate = 2;\n"),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./x"),
        Some(FilePath::new("src/x.ts"))
    );
}
