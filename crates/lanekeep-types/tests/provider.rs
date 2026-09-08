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

use lanekeep_types::{Declaration, Exported};

/// One parsed declaration file, read through the provider the way a real lookup does.
fn declaration(test: &str, source: &str) -> (Project, std::sync::Arc<Declaration>) {
    let project = Project::new(test, &[("d.d.ts", source)]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let decl = provider
        .declaration(&files, &FilePath::new("d.d.ts"))
        .expect("the fixture parses");
    (project, decl)
}

/// Every export shape the grammar produces, one literal source each.
///
/// A table rather than ten functions because the assertion is identical and the *input* is
/// the whole content of each case — and because a kind added to the walk without a row here
/// would be untested in a way no coverage number shows.
#[test]
fn every_declaration_shape_is_found_by_name() {
    for (index, (source, name)) in [
        ("export declare function credit(): number;\n", "credit"),
        ("export declare const rate: number;\n", "rate"),
        ("export type Amount = number;\n", "Amount"),
        ("export interface Order { amount: number }\n", "Order"),
        ("export declare class Decimal {}\n", "Decimal"),
        ("export declare abstract class Base {}\n", "Base"),
        ("declare class Big {}\nexport { Big };\n", "Big"),
        ("declare class Big {}\nexport { Big as Money };\n", "Money"),
        ("export declare enum Currency { USD }\n", "Currency"),
        ("export declare namespace Ns {}\n", "Ns"),
        ("export declare module \"money\" {}\n", "money"),
        ("export function* gen() {}\n", "gen"),
    ]
    .into_iter()
    .enumerate()
    {
        let (_project, decl) = declaration(&format!("shape-{index}"), source);
        assert!(
            matches!(
                lanekeep_types::find_export(&decl, name),
                Some(Exported::Here(_))
            ),
            "`{name}` not declared here in: {source}"
        );
    }
}

/// A re-export names another module rather than a node in this file.
#[test]
fn a_named_re_export_points_at_another_module() {
    let (_project, decl) = declaration(
        "named-re-export",
        "export { Decimal as Money } from './core';\n",
    );
    let Some(Exported::From { specifier, name }) = lanekeep_types::find_export(&decl, "Money")
    else {
        panic!("a named re-export is not a local declaration");
    };
    assert_eq!(specifier, "./core");
    assert_eq!(
        name, "Decimal",
        "the walk follows the exported name, not the alias"
    );
}

/// `export * from` is the fallback, and only the fallback.
///
/// Both halves: a name the file declares itself is **not** answered by the star, and a name
/// it does not declare is. Without the first half, an implementation that always returned the
/// star list would pass — and it would then walk into another file for a name that is right
/// here, which is a different declaration with the same spelling.
#[test]
fn a_star_export_answers_only_a_name_this_file_does_not_declare() {
    let (_project, decl) = declaration(
        "star-export",
        "export * from './a';\nexport * from './b';\nexport declare const rate: number;\n",
    );
    assert!(matches!(
        lanekeep_types::find_export(&decl, "rate"),
        Some(Exported::Here(_))
    ));
    let Some(Exported::Star(sources)) = lanekeep_types::find_export(&decl, "other") else {
        panic!("a name nothing here declares falls to the star sources");
    };
    assert_eq!(
        sources,
        vec!["./a".to_owned(), "./b".to_owned()],
        "source order"
    );
}

#[test]
fn a_namespace_re_export_is_recognized_and_has_no_declaration() {
    let (_project, decl) = declaration("namespace-re-export", "export * as core from './core';\n");
    assert!(matches!(
        lanekeep_types::find_export(&decl, "core"),
        Some(Exported::Namespace { .. })
    ));
}

/// The two spellings of a default export, and the one that has a declared name.
#[test]
fn a_default_export_is_found_under_the_name_default() {
    for (index, (source, declared)) in [
        ("export default class Big {}\n", Some("Big")),
        ("declare class Big {}\nexport default Big;\n", Some("Big")),
        ("declare class Big {}\nexport = Big;\n", Some("Big")),
        ("export default 1;\n", None),
    ]
    .into_iter()
    .enumerate()
    {
        let (_project, decl) = declaration(&format!("default-export-{index}"), source);
        let Some(Exported::Here(node)) = lanekeep_types::find_export(&decl, "default") else {
            panic!("no default export in: {source}");
        };
        assert_eq!(
            lanekeep_types::declared_name(&decl, node),
            declared.map(str::to_owned),
            "{source}"
        );
    }
}

/// `declared_name` unquotes a string-named module, matching `declares()`'s own comparison.
///
/// `declares()` already compares `unquote(text(decl, bound))` against the wanted name, so
/// `find_export` locates `"money"` by its bare spelling either way. `declared_name` read the
/// node's raw text, so asking it what the found declaration is *called* answered `"money"`
/// with the quotes still on — a name nothing outside this file would ever ask for.
#[test]
fn declared_name_unquotes_a_string_named_module() {
    let (_project, decl) =
        declaration("string-module-name", "export declare module \"money\" {}\n");
    let Some(Exported::Here(node)) = lanekeep_types::find_export(&decl, "money") else {
        panic!("a string-named module is found by its unquoted name");
    };
    assert_eq!(
        lanekeep_types::declared_name(&decl, node),
        Some("money".to_owned()),
        "the declared name must not carry the quotes `declares()` already stripped"
    );
}

/// A barrel file: a re-export before a local declaration must not hide the declaration.
///
/// `declared_here` walks every top-level statement; a re-export carries no `declaration`
/// field, and reading that field with `?` returned `None` for the whole file the moment one
/// appeared — the first `export { X } from` in a `.d.ts` made every later local export
/// invisible. Pinned with the shape real barrel files have.
#[test]
fn a_re_export_before_a_local_declaration_does_not_hide_it() {
    let (_project, decl) = declaration(
        "barrel-re-export",
        "export { X } from './other';\ndeclare class Big {}\nexport { Big };\n",
    );
    let Some(Exported::Here(node)) = lanekeep_types::find_export(&decl, "Big") else {
        panic!("a local export after a re-export must still be found");
    };
    assert_eq!(
        lanekeep_types::declared_name(&decl, node),
        Some("Big".to_owned())
    );
}

/// The same barrel shape, reached through the default-export-by-identifier path rather than
/// through `export { Big }` — a second call site into the same `declared_here` walk.
#[test]
fn a_re_export_before_a_default_export_by_identifier_does_not_hide_it() {
    let (_project, decl) = declaration(
        "barrel-re-export-default",
        "export { X } from './other';\ndeclare class Big {}\nexport default Big;\n",
    );
    let Some(Exported::Here(node)) = lanekeep_types::find_export(&decl, "default") else {
        panic!("a default export by identifier after a re-export must still be found");
    };
    assert_eq!(
        lanekeep_types::declared_name(&decl, node),
        Some("Big".to_owned())
    );
}

/// A library `.d.ts` is parsed once per run whatever imports it.
///
/// The claim the declaration cache exists for, asserted through identity rather than through
/// timing: two lookups of one path hand back the same allocation, so nothing between them
/// re-parsed. A timing assertion would be a flake on a loaded machine.
#[test]
fn a_declaration_file_is_parsed_once_per_run() {
    let project = Project::new(
        "declaration-cache",
        &[("lib.d.ts", "export declare const rate: number;\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let path = FilePath::new("lib.d.ts");

    let first = provider.declaration(&files, &path).expect("parses");
    let second = provider.declaration(&files, &path).expect("cached");
    assert!(std::sync::Arc::ptr_eq(&first, &second), "parsed twice");
    assert_eq!(files.dependencies().len(), 1, "read twice");
}

/// A path that is not there is remembered as absent, and remembered once.
#[test]
fn a_missing_declaration_file_is_memoized_as_a_miss() {
    let project = Project::new("declaration-miss", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let path = FilePath::new("lib.d.ts");

    assert!(provider.declaration(&files, &path).is_none());
    assert!(provider.declaration(&files, &path).is_none());
    let reads = files.dependencies();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].hash, None,
        "an absence is a dependency with a null hash"
    );
}

use lanekeep_types::ExportTarget;

/// A chain of re-exports ends at the file and the name that declare the thing.
#[test]
fn a_re_export_chain_ends_at_the_declaring_file_and_name() {
    let project = Project::new(
        "chain",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export { Decimal as Big } from './core';\n",
            ),
            (
                "node_modules/money/core.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let entry = resolve_specifier(&files, &FilePath::new("src/a.ts"), "money").expect("resolves");

    assert_eq!(
        provider.export_target(&files, &entry, "Big"),
        Some(ExportTarget {
            file: FilePath::new("node_modules/money/core.d.ts"),
            name: "Decimal".to_owned(),
        })
    );
}

/// `export *` is followed, in source order, and the first file that has the name wins.
#[test]
fn a_star_re_export_is_followed_in_source_order() {
    let project = Project::new(
        "star-chain",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export * from './a';\nexport * from './b';\n",
            ),
            (
                "node_modules/money/a.d.ts",
                "export declare class Other {}\n",
            ),
            (
                "node_modules/money/b.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let entry = resolve_specifier(&files, &FilePath::new("src/a.ts"), "money").expect("resolves");

    assert_eq!(
        provider.export_target(&files, &entry, "Decimal"),
        Some(ExportTarget {
            file: FilePath::new("node_modules/money/b.d.ts"),
            name: "Decimal".to_owned(),
        })
    );
}

/// A cycle terminates rather than running away, and answers nothing.
///
/// `export * from` in both directions is a shape real packages ship, and the bound alone
/// would only turn an infinite walk into a slow one — the visited set is what makes it fast.
#[test]
fn a_star_re_export_cycle_terminates() {
    let project = Project::new(
        "star-cycle",
        &[
            ("src/a.ts", ""),
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            ("node_modules/money/index.d.ts", "export * from './a';\n"),
            ("node_modules/money/a.d.ts", "export * from './index';\n"),
        ],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let entry = resolve_specifier(&files, &FilePath::new("src/a.ts"), "money").expect("resolves");

    assert_eq!(provider.export_target(&files, &entry, "Missing"), None);
}

/// A file that imports its own name terminates rather than looping.
///
/// `export_target`'s visited set is what stops it: the walk marks `(src/a.d.ts, "A")` visited
/// before it can be asked for again, so a second visit answers `None` instead of recursing.
/// This never actually reaches that second visit here — `find_export` finds `A` declared
/// locally (`export type A = number`) on the first visit and returns it directly, so the
/// self-import statement is never followed at all — but it is the visited set, not the depth
/// bound, that would stop it if a future declaration shape made the walk revisit the file.
#[test]
fn a_self_importing_declaration_file_terminates() {
    let project = Project::new(
        "self-import",
        &[(
            "src/a.d.ts",
            "import { A } from './a';\nexport type A = number;\n",
        )],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let file = FilePath::new("src/a.d.ts");

    assert_eq!(
        provider.export_target(&files, &file, "A"),
        Some(ExportTarget {
            file: FilePath::new("src/a.d.ts"),
            name: "A".to_owned(),
        })
    );
}

/// Two files whose type aliases name each other terminates rather than looping.
///
/// `a.d.ts` says `A` is `B`, `b.d.ts` says `B` is `A`; nothing here ever bottoms out at a
/// concrete type. `MAX_DEPTH` is what stops it — a real bound, not a performance claim, so
/// this asserts it returns promptly rather than merely asserting it returns an answer. The
/// bound does not answer `None`: it stops mid-chain and reports the last alias reached as a
/// nominal type, exactly as running out of budget partway through a same-file chain would.
#[test]
fn a_mutual_alias_cycle_across_two_files_terminates() {
    let project = Project::new(
        "mutual-alias-cycle",
        &[
            (
                "src/a.d.ts",
                "import { B } from './b';\nexport type A = B;\n",
            ),
            (
                "src/b.d.ts",
                "import { A } from './a';\nexport type B = A;\n",
            ),
        ],
    );
    let subject = "import { A } from './a';\nlet x: A;\n";
    let started = std::time::Instant::now();
    let result = ask(&project, subject, TypeProvider::type_of);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "`MAX_DEPTH` must stop the walk well inside the budget, not merely before it hangs"
    );
    assert_eq!(
        result,
        Some(lanekeep_types::Type::Nominal {
            name: "B".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "B".to_owned(),
                module: Some("./b".to_owned()),
                exported: Some("B".to_owned()),
            }),
        }),
        "the bound stops mid-chain rather than resolving to a concrete type"
    );
}

/// A named re-export of a name that does not exist anywhere answers nothing.
#[test]
fn a_re_export_of_a_name_nothing_declares_answers_nothing() {
    let project = Project::new(
        "chain-dead-end",
        &[
            ("lib.d.ts", "export { Gone } from './core';\n"),
            ("core.d.ts", "export declare class Decimal {}\n"),
        ],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    assert_eq!(
        provider.export_target(&files, &FilePath::new("lib.d.ts"), "Gone"),
        None
    );
}

/// A project, a provider, and one question about `src/a.ts`.
///
/// Every cross-file case below is the same three lines otherwise, and the interesting part of
/// each is its fixture — so the harness is one helper and the tests are their sources.
fn ask<T>(
    project: &Project,
    subject: &str,
    ask: impl FnOnce(&lanekeep_types::BuiltinProvider, Query<'_>) -> T,
) -> T {
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "identifier");
    ask(
        &provider,
        Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files,
        },
    )
}

/// An imported value is typed from its declaration file.
#[test]
fn an_imported_value_is_typed_through_its_declaration_file() {
    let project = Project::new(
        "imported-value",
        &[("src/money.d.ts", "export declare const rate: number;\n")],
    );
    let subject = "import { rate } from './money';\nconst y = rate;\n";
    assert_eq!(
        ask(&project, subject, TypeProvider::type_of),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        ))
    );
}

/// An imported alias is transparent, exactly as a same-file one is.
#[test]
fn an_imported_type_alias_resolves_to_what_it_aliases() {
    let project = Project::new(
        "imported-alias",
        &[("src/money.d.ts", "export type Amount = number;\n")],
    );
    let subject = "import { Amount } from './money';\nlet x: Amount;\n";
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_annotation");
    assert_eq!(
        provider.type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files
        }),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        ))
    );
}

/// An imported *class* keeps its use-site module, and gains its declared name.
///
/// The half that would be a false positive if the arm answered with the declaration's own
/// symbol: `lanekeep/no-restricted-types` matches on `module`, and a declaration read out of
/// `node_modules` is a local declaration in *that* file with no module at all.
#[test]
fn an_imported_class_keeps_its_module_and_gains_its_exported_name() {
    let project = Project::new(
        "imported-class",
        &[
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export { Decimal as Big } from './core';\n",
            ),
            (
                "node_modules/money/core.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let subject = "import { Big } from 'money';\nlet x: Big;\n";
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_annotation");
    assert_eq!(
        provider.type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files
        }),
        Some(lanekeep_types::Type::Nominal {
            name: "Big".to_owned(),
            symbol: Some(lanekeep_types::Symbol {
                name: "Big".to_owned(),
                module: Some("money".to_owned()),
                exported: Some("Decimal".to_owned()),
            }),
        })
    );
}

/// `symbolOf` follows the chain to the declared name and keeps the specifier as written.
#[test]
fn symbol_of_reports_the_declared_name_and_the_specifier_as_written() {
    let project = Project::new(
        "symbol-chain",
        &[
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export { Decimal as Big } from './core';\n",
            ),
            (
                "node_modules/money/core.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let subject = "import { Big } from 'money';\nconst y = Big;\n";
    assert_eq!(
        ask(&project, subject, TypeProvider::symbol_of),
        Some(lanekeep_types::Symbol {
            name: "Big".to_owned(),
            module: Some("money".to_owned()),
            exported: Some("Decimal".to_owned()),
        })
    );
}

/// An unreadable declaration file leaves the import exactly where it was.
///
/// The silence posture, asserted head-on: `module` still comes from the import statement, and
/// `exported` falls back to what that statement says rather than to nothing. A rule matching
/// on the module keeps working on a project whose `node_modules` is not installed.
#[test]
fn an_unresolvable_import_falls_back_to_what_the_import_statement_says() {
    let project = Project::new("symbol-unresolvable", &[]);
    let subject = "import { Decimal } from 'money';\nconst y = Decimal;\n";
    assert_eq!(
        ask(&project, subject, TypeProvider::symbol_of),
        Some(lanekeep_types::Symbol {
            name: "Decimal".to_owned(),
            module: Some("money".to_owned()),
            exported: Some("Decimal".to_owned()),
        })
    );
}

/// A namespace import binds the module object, which has no one exported name.
#[test]
fn a_namespace_import_has_no_exported_name() {
    let project = Project::new(
        "symbol-namespace",
        &[
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            (
                "node_modules/money/index.d.ts",
                "export declare class Decimal {}\n",
            ),
        ],
    );
    let subject = "import * as money from 'money';\nconst y = money;\n";
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(symbol.module.as_deref(), Some("money"));
    assert_eq!(symbol.exported, None);
}

/// A chain deeper than the bound stops rather than running away.
///
/// Every hop forwards `Decimal` under the same name except the last-but-one, which renames it
/// to `Real` on the way to a final hop that declares `Real`. That rename is load-bearing: with
/// every hop keeping the same name, the bounded walk's fallback (`Decimal`, the import
/// statement's own spelling) and the unbounded walk's true answer (also `Decimal`) coincide, so
/// the assertion below would pass whether or not the bound did anything at all — confirmed by
/// setting `MAX_EXPORT_DEPTH` to `100_000` and watching this test fail once the rename is in
/// place, since the walk then reaches the real declaration and answers `Real`.
#[test]
fn a_re_export_chain_past_the_bound_answers_nothing() {
    let mut files: Vec<(String, String)> = Vec::new();
    // Twenty hops, four past `MAX_EXPORT_DEPTH`. Every hop up to hop19 only forwards `Decimal`;
    // hop19 renames it to `Real` on the way to hop20, which declares `Real`.
    for hop in 0..19 {
        files.push((
            format!("src/hop{hop}.d.ts"),
            format!("export {{ Decimal }} from './hop{}';\n", hop + 1),
        ));
    }
    files.push((
        "src/hop19.d.ts".to_owned(),
        "export { Real as Decimal } from './hop20';\n".to_owned(),
    ));
    files.push((
        "src/hop20.d.ts".to_owned(),
        "export declare class Real {}\n".to_owned(),
    ));
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let project = Project::new("chain-bound", &borrowed);

    let subject = "import { Decimal } from './hop0';\nconst y = Decimal;\n";
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(
        symbol.exported.as_deref(),
        Some("Decimal"),
        "the bound stops the walk before the rename, and the fallback is what the import \
         statement says"
    );
}

/// The same shape, short enough to finish: the walk crosses the rename and reports it.
///
/// The half that proves the test above is measuring the bound and not just a broken walk —
/// with the identical rename-then-declare shape but only five hops, the answer is the real
/// declared name rather than the fallback.
#[test]
fn a_re_export_chain_within_the_bound_answers_the_renamed_declaration() {
    let mut files: Vec<(String, String)> = Vec::new();
    for hop in 0..3 {
        files.push((
            format!("src/hop{hop}.d.ts"),
            format!("export {{ Decimal }} from './hop{}';\n", hop + 1),
        ));
    }
    files.push((
        "src/hop3.d.ts".to_owned(),
        "export { Real as Decimal } from './hop4';\n".to_owned(),
    ));
    files.push((
        "src/hop4.d.ts".to_owned(),
        "export declare class Real {}\n".to_owned(),
    ));
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let project = Project::new("chain-within-bound", &borrowed);

    let subject = "import { Decimal } from './hop0';\nconst y = Decimal;\n";
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(
        symbol.exported.as_deref(),
        Some("Real"),
        "within the bound the walk crosses the rename and answers the real declaration"
    );
}
