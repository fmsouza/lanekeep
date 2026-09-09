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

use std::fmt::Write as _;
use std::path::PathBuf;

use lanekeep_core::{AnalysisBudget, FileAccess, FilePath};
use lanekeep_lang_js::{Tsx, TypeScript};
use lanekeep_types::{Query, TypeProvider};

/// A budget generous enough that nothing here can breach it.
///
/// `begin_run` takes one because a provider that spends wall clock preparing must spend the
/// run's budget; the two providers exercised in this file do no work there, so the value only
/// has to be a value.
fn budget() -> AnalysisBudget {
    AnalysisBudget::start(std::time::Duration::from_mins(10))
}

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

/// The default `begin_run` never walks the corpus.
///
/// The file list is lazy — `&dyn Fn() -> Vec<FilePath>` rather than `&[FilePath]` — because
/// the engine's only way to produce one is a second walk of the whole project, and the only
/// provider that ships ignores it. A warm run paying for a corpus walk nothing reads is a cost
/// with no answer attached to it.
#[test]
fn the_default_begin_run_does_not_walk_the_corpus() {
    struct Silent;
    impl TypeProvider for Silent {
        fn type_of(&self, _: Query<'_>) -> Option<lanekeep_types::Type> {
            None
        }
        fn symbol_of(&self, _: Query<'_>) -> Option<lanekeep_types::Symbol> {
            None
        }
        fn return_type_of(&self, _: Query<'_>) -> Option<lanekeep_types::Type> {
            None
        }
        fn is_assignable_to(&self, _: Query<'_>, _: &str, _: &str) -> Option<bool> {
            None
        }
        fn complete(&self, _: Query<'_>) -> bool {
            true
        }
        fn identity(&self) -> Vec<u8> {
            Vec::new()
        }
    }

    let walked = std::cell::Cell::new(false);
    let files = || {
        walked.set(true);
        Vec::new()
    };
    assert_eq!(Silent.begin_run(&files, budget()), Ok(Vec::new()));
    assert!(
        !walked.get(),
        "the default body must not ask for a file list it does not read"
    );
}

/// A path that answered nothing is parsed once it becomes text, within one run.
///
/// A memo of the failures was consulted *before* the read, so a path probed while it was
/// absent stayed absent for the rest of the run however the filesystem moved under it — and a
/// miss carries no hash, so nothing about the entry could ever say otherwise. Asking for the
/// hash first is what closes it, and it is why nothing memoizes a failure at all now: a second
/// probe is one `hash_of`, which the access answers from its own memo.
///
/// `begin_run` no longer forces a reparse: the declaration answers a second run without ever
/// having been dropped, because it is kept by hash rather than cleared wholesale (Task 6.1's
/// fix round). Before that, the assertion below passed for a different reason — `begin_run`
/// cleared the memo and a *cold* re-read happened to find the same file — so this is the same
/// black-box check the earlier version made, retitled to the contract it actually exercises;
/// `crates/lanekeep-types/src/builtin.rs`'s own unit tests are what can see the memo directly
/// and assert the file is not reparsed.
#[test]
fn a_path_that_was_absent_is_parsed_once_it_becomes_text() {
    let project = Project::new("miss-becomes-text", &[]);
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let path = FilePath::new("lib.d.ts");

    assert!(
        provider.declaration(&project.files(), &path).is_none(),
        "nothing is there yet"
    );
    project.write("lib.d.ts", "export declare class Big {}\n");
    let parsed = provider
        .declaration(&project.files(), &path)
        .expect("a path that has become text is read rather than held at the old answer");
    assert!(lanekeep_types::declared_here(&parsed, "Big").is_some());

    provider
        .begin_run(&Vec::new, budget())
        .expect("a run begins");
    assert!(
        provider.declaration(&project.files(), &path).is_some(),
        "and a held declaration answers a second run, whether by surviving begin_run or by \
         being re-read cold — either way, nothing here can stay missing forever"
    );
}

/// Unlike the declaration memo above, completeness *is* cleared every run.
///
/// It carries no hash to compare against — a verdict over a whole file's imports, not a
/// single read — so a held provider (#191) has no way to tell a stale entry from a current
/// one except by forgetting it. It is the one a rule reads directly, too: a file that was
/// incomplete because a declaration was missing must be re-decided once the declaration
/// exists, and nothing but this clearing can make that happen for a held provider.
#[test]
fn begin_run_clears_a_held_providers_completeness_memo() {
    let project = Project::new("begin-run-clears-completeness", &[]);
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { rate } from './money';\nconst x = rate;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let question = |files: &FileAccess| {
        provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files,
        })
    };

    assert!(
        !question(&project.files()),
        "the import resolves to nothing"
    );
    project.write("src/money.d.ts", "export declare const rate: number;\n");
    assert!(
        !question(&project.files()),
        "the answer is memoized per file within a run"
    );

    provider
        .begin_run(&Vec::new, budget())
        .expect("a run begins");
    assert!(
        question(&project.files()),
        "a run starts cold, so completeness is decided again against the filesystem now"
    );
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

/// TypeScript's ESM spelling of a `.tsx` module names the emitted `.jsx`, and the source sits
/// at the same stem — the `.js` case one probe over, for the file the tsx grammar reads.
#[test]
fn a_specifier_naming_the_emitted_jsx_resolves_to_its_tsx_source() {
    let project = Project::new(
        "relative-jsx-suffix",
        &[
            ("src/a.ts", ""),
            (
                "src/Button.tsx",
                "export const who: string = 'b';\nexport const B = () => <b/>;\n",
            ),
        ],
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "./Button.jsx"),
        Some(FilePath::new("src/Button.tsx"))
    );
    let provider = lanekeep_types::BuiltinProvider::probe_with(&TypeScript, Some(&Tsx))
        .expect("TypeScript and tsx");
    let subject = "import { who } from './Button.jsx';\nlet w = who;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "identifier");
    assert_eq!(
        provider.type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files,
        }),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::String
        )),
        "the `.jsx` spelling reaches the `.tsx` source and its own annotation"
    );
}

/// A `.tsx` sibling resolves and parses with the TSX grammar.
///
/// The refusal this replaces was `RELATIVE_SUFFIXES`' deliberate omission of `.tsx`: one
/// parser could speak only the TypeScript grammar, under which every JSX element is a
/// silent `ERROR` node. With a second parser, chosen by the resolved path's extension,
/// the sibling reads honestly and the importer's imports all resolve.
#[test]
fn a_tsx_sibling_is_resolved_and_parsed_with_the_tsx_grammar() {
    let project = Project::new(
        "relative-tsx-resolves",
        &[("src/Button.tsx", "export const B = () => <b/>;\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe_with(&TypeScript, Some(&Tsx))
        .expect("TypeScript and tsx");
    let subject = "import { B } from './Button';\nlet b = B;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/app.tsx");
    assert!(
        provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the .tsx sibling resolves and its JSX parses without an ERROR"
    );
}

/// Without a tsx grammar the provider still refuses `.tsx` — honestly, as an incomplete
/// file, rather than parsing JSX into silent `ERROR` nodes and answering from them.
#[test]
fn a_tsx_sibling_stays_unresolved_without_a_tsx_grammar() {
    let project = Project::new(
        "relative-tsx-refuses",
        &[(
            "src/Button.tsx",
            "export const label: string = 'hi';\nexport const B = () => <b/>;\n",
        )],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    // The clean export, deliberately: under the TypeScript grammar the JSX statement is the
    // `ERROR`, and a per-declaration verdict would find `label` untouched and call the file
    // read — through a parse in the wrong dialect.
    let subject = "import { label } from './Button';\nlet l = label;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/app.tsx");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "no tsx grammar, no honest read of the sibling"
    );
}

/// The extension is matched case-insensitively, so a manifest that names a `Button.TSX`
/// still gets the TSX parser. Relative probes append lowercase suffixes, so the case can
/// only reach the parser through a manifest-declared path — and the import is nameless so
/// the whole-file verdict is what the grammar's own parse decides.
#[test]
fn an_uppercase_tsx_extension_is_parsed_with_the_tsx_grammar() {
    let project = Project::new(
        "relative-tsx-uppercase",
        &[
            (
                "node_modules/widgets/package.json",
                r#"{"exports": {"./Button": "./src/Button.TSX"}}"#,
            ),
            (
                "node_modules/widgets/src/Button.TSX",
                "export const B = () => <b/>;\n",
            ),
        ],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe_with(&TypeScript, Some(&Tsx))
        .expect("TypeScript and tsx");
    let subject = "import 'widgets/Button';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/app.tsx");
    assert!(
        provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the uppercase extension resolves, and its JSX parses with the TSX grammar"
    );
}

/// A type answer crosses into the sibling, typed from its own annotation.
#[test]
fn a_type_answer_crosses_into_a_tsx_sibling() {
    let project = Project::new(
        "tsx-sibling-type",
        &[(
            "src/Button.tsx",
            "export const who: string = 'b';\nexport const B = () => <b/>;\n",
        )],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe_with(&TypeScript, Some(&Tsx))
        .expect("TypeScript and tsx");
    let subject = "import { who } from './Button';\nlet w = who;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "identifier");
    assert_eq!(
        provider.type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files,
        }),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::String
        )),
        "`who` is typed by its own annotation in the .tsx sibling"
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
        .map(|read| (read.path.as_str().to_owned(), read.hash().is_some()))
        .collect();
    assert_eq!(
        recorded,
        vec![
            ("src/money.cts".to_owned(), false),
            ("src/money.d.ts".to_owned(), false),
            ("src/money.mts".to_owned(), false),
            ("src/money.ts".to_owned(), false),
            ("src/money.tsx".to_owned(), false),
            ("src/money/index.d.ts".to_owned(), false),
            ("src/money/index.ts".to_owned(), false),
            ("src/money/index.tsx".to_owned(), false),
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

/// A `types` condition that is itself a conditions object resolves through it.
///
/// `{"types": {"import": "./index.d.mts", "require": "./index.d.ts"}}` is the shape a package
/// shipping both module systems publishes, and it answered nothing: the nested search only
/// recursed into further objects, so an object of string leaves fell through both arms. The
/// package then resolved by the `index.d.ts` fallback or not at all, and a package whose
/// declarations live anywhere else was simply unreadable.
#[test]
fn a_types_condition_holding_conditions_resolves_to_a_string_leaf() {
    let project = project_with(
        "bare-nested-types",
        &package(
            r#"{"exports": {".": {"types": {"import": "./build/index.d.ts", "require": "./nope.d.ts"}}}}"#,
        ),
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        Some(FilePath::new("node_modules/money/build/index.d.ts")),
        "the conditions under `types` are read in the fixed key order the whole function uses"
    );
}

/// And a conditions object with no `types` anywhere still answers nothing.
///
/// The half that must not move: `default` and `import` outside a `types` condition name the
/// *emitted JavaScript*, and following one would hand this provider a `.js` file to parse as
/// TypeScript. A confidently wrong type is worse than none.
#[test]
fn conditions_with_no_types_condition_still_resolve_to_nothing() {
    let project = project_with(
        "bare-no-types-condition",
        &package(r#"{"exports": {".": {"import": "./build/index.d.ts", "default": "./x.js"}}}"#),
    );
    let files = project.files();
    assert_eq!(
        resolve_specifier(&files, &FilePath::new("src/a.ts"), "money"),
        None
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
        assert_eq!(read.hash(), None, "every in-root candidate is absent");
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
        reads[0].hash(),
        None,
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

/// When two `export *` sources both declare the same name, the first in source order wins.
///
/// Addendum E (task 4.16, from Task 8's review, optional). The test above shows the first
/// source that *has* the name winning over one that lacks it entirely — which a walk that
/// stopped at the first star clause regardless of content would also pass. This is the case
/// that actually needs the "first" half of the claim: both `a.d.ts` and `b.d.ts` declare
/// `Decimal`, so only checking source order — not merely presence — tells them apart.
#[test]
fn a_collision_between_two_star_sources_is_won_by_the_first_in_source_order() {
    let project = Project::new(
        "star-collision",
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
                "export declare class Decimal {}\n",
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
            file: FilePath::new("node_modules/money/a.d.ts"),
            name: "Decimal".to_owned(),
        }),
        "both files declare `Decimal`; the first `export *` clause in source order wins"
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
/// this asserts it returns promptly rather than merely asserting it returns an answer.
///
/// Addendum B (task 4.16) changed what the bound answers. Before: it stopped mid-chain and
/// reported the last alias reached as a nominal type — a *confident* answer carrying an
/// intermediate file's own specifier, which `lanekeep/no-restricted-types` then reported as
/// a false positive, because that specifier is not what the program actually named. Now: a
/// chain cut by the bound answers `None`, on the same reasoning a same-file chain already
/// used running out of budget partway through — a cut answer is unknown, never a guess.
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
        result, None,
        "the bound cuts the chain, and a cut answer is unknown rather than a guess"
    );
}

/// A single alias chain, twenty files deep, answers nothing rather than a guess.
///
/// Addendum B. Unlike the mutual cycle above, this chain is acyclic and really does bottom
/// out at a concrete type (`export type X = number;` in the last file) — but only after more
/// hops than `MAX_DEPTH` allows, so the walk never gets there. The pre-fix code would have
/// answered a nominal type naming whichever file the sixteenth hop happened to land in; this
/// asserts the honest answer instead.
#[test]
fn an_alias_chain_past_the_depth_bound_answers_nothing() {
    const HOPS: usize = 20;
    let mut files: Vec<(String, String)> = (0..HOPS)
        .map(|i| {
            (
                format!("src/a{i}.d.ts"),
                format!("import {{ X }} from './a{}';\nexport type X = X;\n", i + 1),
            )
        })
        .collect();
    files.push((
        format!("src/a{HOPS}.d.ts"),
        "export type X = number;\n".to_owned(),
    ));
    let owned: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    let project = Project::new("alias-chain-too-deep", &owned);
    let subject = "import { X } from './a0';\nlet x: X;\n";
    let started = std::time::Instant::now();
    let result = ask(&project, subject, TypeProvider::type_of);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "`MAX_DEPTH` must stop the walk well inside the budget, not merely before it hangs"
    );
    assert_eq!(
        result, None,
        "twenty hops exceeds `MAX_DEPTH`, so the concrete `number` at the end is never reached"
    );
}

/// The depth bound alone, with no cycle anywhere to reach it.
///
/// `walk_export` has two stopping conditions and the tests around it exercise the *visited*
/// set — `export *` in both directions, a self-import, a mutual alias. A chain of distinct
/// files trips neither of those: only the bound stops it, and a bound deleted from a walk that
/// still has a cycle guard leaves every one of those tests green while an over-long chain
/// walks the whole way. Both sides are asserted, one hop apart, so the rows pin the bound's
/// value rather than merely its existence.
#[test]
fn a_chain_longer_than_the_export_depth_bound_answers_nothing() {
    for (test, declared_at, expected) in [
        ("chain-depth-inside", 15, true),
        ("chain-depth-outside", 16, false),
    ] {
        let mut files: Vec<(String, String)> = (0..declared_at)
            .map(|hop| {
                (
                    format!("src/hop{hop}.d.ts"),
                    format!("export {{ Decimal }} from './hop{}';\n", hop + 1),
                )
            })
            .collect();
        files.push((
            format!("src/hop{declared_at}.d.ts"),
            "export declare class Decimal {}\n".to_owned(),
        ));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let project = Project::new(test, &borrowed);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");

        assert_eq!(
            provider
                .export_target(&files, &FilePath::new("src/hop0.d.ts"), "Decimal")
                .is_some(),
            expected,
            "a chain of {declared_at} distinct hops, with no cycle in it anywhere"
        );
    }
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

/// A destructured export binds a pattern, and the pattern's text is not a name.
///
/// The declaration walk finds `export const { e } = { e: 2 }` for the name `e` — the
/// resolver matches through the pattern — but the thing it found has no *name*: the
/// declarator's `name` field is the whole `object_pattern`, and reporting `{ e }` where a
/// spelling belongs would hand a rule comparing `exported` against a required name a
/// mismatch on conforming code. The chain falls back to the asked name, which is what a
/// shorthand destructuring exports.
#[test]
fn a_destructured_export_falls_back_to_the_asked_name() {
    let project = Project::new(
        "symbol-destructured",
        &[("src/pieces.d.ts", "export const { e } = { e: 2 };\n")],
    );
    let subject = "import { e } from './pieces';\nconst y = e;\n";
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(
        symbol.exported.as_deref(),
        Some("e"),
        "the pattern's text is not a name; the asked spelling is"
    );
}

/// `returnTypeOf` over one file, one case per rule in §3.5.
///
/// A table, because the rule is stated as five clauses and each needs its own row: an
/// annotated signature, a single inferred return, several returns that agree, several that
/// cannot all be typed, and a function with no return at all.
#[test]
fn return_type_of_reads_a_signature_or_infers_one() {
    for (subject, expected) in [
        (
            "function rate(): number { return compute(); }\nrate();\n",
            Some(lanekeep_types::Type::Primitive(
                lanekeep_types::Primitive::Number,
            )),
        ),
        (
            "function rate() { return 1; }\nrate();\n",
            Some(lanekeep_types::Type::Primitive(
                lanekeep_types::Primitive::Number,
            )),
        ),
        (
            "function rate(f) { if (f) { return 1; } return 'a'; }\nrate(1);\n",
            lanekeep_types::Type::union(vec![
                lanekeep_types::Type::Primitive(lanekeep_types::Primitive::Number),
                lanekeep_types::Type::Primitive(lanekeep_types::Primitive::String),
            ]),
        ),
        // One member the oracle cannot type sinks the whole union, exactly as an annotation's
        // union already does: a partial answer is byte-identical to a complete one, and a
        // rule reporting on it would accuse code a member of which it never read.
        (
            "function rate(f) { if (f) { return 1; } return f ?? 2; }\nrate(1);\n",
            None,
        ),
        // No `return` at all answers nothing rather than `void`: this oracle has no `void`,
        // and inventing one would be a claim about a function it read only the shape of.
        ("function rate() { compute(); }\nrate();\n", None),
        // A concise arrow body is the returned expression itself.
        (
            "const rate = () => 1;\nrate();\n",
            Some(lanekeep_types::Type::Primitive(
                lanekeep_types::Primitive::Number,
            )),
        ),
    ] {
        let project = Project::new("return-local", &[]);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let tree = parse(subject);
        let file = FilePath::new("src/a.ts");
        let node = last_of(&tree, "call_expression");
        assert_eq!(
            provider.return_type_of(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &files
            }),
            expected,
            "{subject}"
        );
    }
}

/// A method's own declaration answers its return type directly, with no call needed.
///
/// Addendum A1: `return_type_at`'s dispatch already lists `method_definition` among the
/// function-like kinds `signature_return` reads — this is the row that was missing to prove
/// it, since the table above only ever asks through a `call_expression`.
#[test]
fn return_type_of_reads_a_method_definitions_own_declaration() {
    let project = Project::new("return-method-definition", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "class C { m(): number { return 1; } }\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "method_definition");
    assert_eq!(
        provider.return_type_of(Query {
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

/// A `method_signature` inside an interface answers its declared return type.
///
/// Addendum A1. An interface member has no body, so the annotation is the only source
/// `signature_return` can read — this is the row that proves the arm is reached at all.
#[test]
fn return_type_of_reads_a_method_signature_inside_an_interface() {
    let project = Project::new("return-method-signature", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "interface I { m(): number; }\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "method_signature");
    assert_eq!(
        provider.return_type_of(Query {
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

/// An `abstract_method_signature` inside an abstract class answers its declared return type.
///
/// Addendum A1, the pair of the interface row above — an abstract member has no body either.
#[test]
fn return_type_of_reads_an_abstract_method_signature() {
    let project = Project::new("return-abstract-method-signature", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "abstract class A { abstract m(): number; }\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "abstract_method_signature");
    assert_eq!(
        provider.return_type_of(Query {
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

/// An arrow function's explicit return annotation wins over what its body would infer.
///
/// Addendum A1. The table above has an arrow row with no annotation at all
/// (`const rate = () => 1`), so the annotation-wins precedence — already asserted for a
/// `function_declaration` two rows up — was never asserted for an arrow specifically. The
/// body here returns a `string`; only the annotation winning explains the `number` answer.
#[test]
fn return_type_of_prefers_an_arrow_functions_own_annotation() {
    let project = Project::new("return-arrow-annotation", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "const rate = (): number => 'a';\nrate();\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    assert_eq!(
        provider.return_type_of(Query {
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

/// Calling a value that is not a function answers nothing, rather than guessing.
///
/// Addendum A1. `x`'s declaration is a number literal, which is none of the kinds
/// `return_type_at` recognizes as function-like — the fall-through `_ => None` is what a
/// rule sees, not a panic and not a number that was never returned.
#[test]
fn return_type_of_a_call_to_a_non_function_value_answers_nothing() {
    let project = Project::new("return-non-function", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "const x = 5;\nx();\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    assert_eq!(
        provider.return_type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files
        }),
        None
    );
}

/// A generator function *expression*, bound to a name, answers nothing without an annotation.
///
/// Addendum A1's second half is still what the fixture is for: `is_function_like` already
/// lists `generator_function` (the expression form assigned by `const g = function*() {...}`),
/// but `return_type_at`'s own dispatch listed only `generator_function_declaration` — so a
/// call to a generator bound this way fell through to `_ => None` despite the oracle
/// considering it function-like everywhere else. The *answer* changed: a generator's declared
/// value is a `Generator<…>` and the body's `return` is not it, so an unannotated generator
/// answers nothing rather than the body's type. The annotated row below is what still pins the
/// dispatch.
#[test]
fn return_type_of_reads_a_generator_function_expression() {
    let project = Project::new("return-generator-expression", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "const g = function*() { return 1; };\ng();\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    assert_eq!(
        provider.return_type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files
        }),
        None,
        "calling a generator yields a generator object, not what its body returns"
    );
}

/// An annotated one answers its annotation, which is what pins the dispatch.
#[test]
fn return_type_of_reads_an_annotated_generator_function_expression() {
    let project = Project::new("return-generator-annotated", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "const g = function*(): number { return 1; };\ng();\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    assert_eq!(
        provider.return_type_of(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &files
        }),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        )),
        "the annotation is what the program means, generator or not"
    );
}

/// An `async` function with no annotation answers nothing rather than the body's type.
///
/// `async function rate() { return 1 }` returns a `Promise<number>`, and this oracle has no
/// variant that can say so — no type arguments, no `Promise`. Answering `number` would be a
/// claim about a value a rule can compare against a `number` and be wrong every time, and the
/// failure is silent: nothing about the answer says a wrapper was dropped. Three spellings,
/// because the marker sits on three different node kinds.
#[test]
fn return_type_of_an_unannotated_async_function_answers_nothing() {
    for subject in [
        "async function rate() { return 1; }\nrate();\n",
        "const rate = async () => 1;\nrate();\n",
        "const rate = async function () { return 1; };\nrate();\n",
        "async function* rate() { return 1; }\nrate();\n",
    ] {
        let project = Project::new("return-async", &[]);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let tree = parse(subject);
        let file = FilePath::new("src/a.ts");
        let node = last_of(&tree, "call_expression");
        assert_eq!(
            provider.return_type_of(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &files
            }),
            None,
            "{subject}"
        );
    }
}

/// And an annotated `async` function answers its annotation, exactly as written.
///
/// The pair the row above needs: the refusal is about the *absence* of an annotation, not
/// about `async`, so a rule that annotates its promises still gets an answer.
#[test]
fn return_type_of_an_annotated_async_function_answers_the_annotation() {
    let project = Project::new("return-async-annotated", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "async function rate(): number { return 1; }\nrate();\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    assert_eq!(
        provider.return_type_of(Query {
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

/// The case `no-query-result-leak` needs: a call into a library, resolved through its
/// `function_signature`, answered by name with type arguments dropped.
#[test]
fn return_type_of_resolves_an_imported_signature() {
    let project = Project::new(
        "return-imported",
        &[
            (
                "node_modules/@tanstack/react-query/package.json",
                r#"{"exports": {".": {"types": "./build/index.d.ts"}}}"#,
            ),
            (
                "node_modules/@tanstack/react-query/build/index.d.ts",
                "export declare class UseQueryResult {}\n\
                 export declare function useQuery(o: unknown): UseQueryResult;\n",
            ),
        ],
    );
    let subject = "import { useQuery } from '@tanstack/react-query';\nuseQuery({});\n";
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "call_expression");
    let Some(lanekeep_types::Type::Nominal { name, symbol }) = provider.return_type_of(Query {
        file: &file,
        tree: &tree,
        source: subject,
        node,
        files: &files,
    }) else {
        panic!("the library's signature names a class");
    };
    assert_eq!(name, "UseQueryResult");
    // Declared in the library file rather than imported into it, so it carries no module —
    // which is the honest report: the name is the library's own, at its own site.
    assert_eq!(symbol.and_then(|s| s.module), None);
}

/// One package exporting a base class and a base interface, for the heritage cases.
const HERITAGE_PACKAGE: &[(&str, &str)] = &[
    (
        "node_modules/money/package.json",
        r#"{"types": "./index.d.ts"}"#,
    ),
    (
        "node_modules/money/index.d.ts",
        "export declare class Decimal {}\n\
         export declare interface Amountish { amount: number }\n\
         export type Money = Decimal;\n\
         export type Amount = number;\n",
    ),
];

/// Ask `isAssignableTo` about the last annotated declarator in `subject`, against `("money",
/// "Decimal")`.
fn assignable(test: &str, extra: &[(&str, &str)], subject: &str) -> Option<bool> {
    assignable_target(test, extra, subject, "money", "Decimal")
}

/// Ask `isAssignableTo` about the last annotated declarator in `subject`, against an
/// arbitrary `(module, name)` target.
fn assignable_target(
    test: &str,
    extra: &[(&str, &str)],
    subject: &str,
    module: &str,
    name: &str,
) -> Option<bool> {
    let mut files: Vec<(&str, &str)> = HERITAGE_PACKAGE.to_vec();
    files.extend_from_slice(extra);
    let project = Project::new(test, &files);
    let access = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_annotation");
    provider.is_assignable_to(
        Query {
            file: &file,
            tree: &tree,
            source: subject,
            node,
            files: &access,
        },
        module,
        name,
    )
}

/// A heritage graph that re-converges is walked once per node, not once per path.
///
/// The `visited` set is path-scoped — deliberately, so a sibling branch reaching the same
/// ancestor is not told it was already walked — and that cuts cycles without cutting
/// *re-convergence*: twelve levels of four parents each is 4^12 ≈ 16.8 million paths through
/// forty-eight declarations. A host call cannot be interrupted, so a run would sit inside one
/// `isAssignableTo` with nothing able to stop it. A per-call memo makes it forty-eight.
///
/// **The graph must not reach the target.** `heritage_assignable` returns on the first parent
/// that answers `true`, so an assignable diamond is answered by one path and pins nothing at
/// all — measured: the `Some(true)` shape this was first written as ran in 20 ms against the
/// unmemoized walk. Only the negative answer has to visit every path, which is why the
/// assertion below is `Some(false)` rather than the `Some(true)` the review proposed.
///
/// A termination line rather than a benchmark: the figure is orders of magnitude away from
/// either answer, so it says "polynomial" without pinning a machine's speed.
#[test]
fn a_reconverging_heritage_graph_is_walked_once_per_declaration() {
    const WIDTH: usize = 4;
    const DEPTH: usize = 12;

    let mut source = String::from("import { Decimal } from 'money';\ninterface Base {}\n");
    for level in 0..DEPTH {
        for node in 0..WIDTH {
            let parents: Vec<String> = if level + 1 == DEPTH {
                vec!["Base".to_owned()]
            } else {
                (0..WIDTH).map(|p| format!("N{}_{p}", level + 1)).collect()
            };
            let _ = writeln!(
                source,
                "interface N{level}_{node} extends {} {{}}",
                parents.join(", ")
            );
        }
    }
    source.push_str("let x: N0_0;\n");

    let started = std::time::Instant::now();
    let answer = assignable("assignable-reconverging", &[], &source);
    let elapsed = started.elapsed();
    assert_eq!(
        answer,
        Some(false),
        "the graph is fully readable and reaches `Base` rather than the named type"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the walk must be per declaration rather than per path: took {elapsed:?}"
    );
}

/// And the same graph rooted at the named type is still assignable.
///
/// The memo must not turn a reachable target into a miss: an entry answered `true` anywhere is
/// the answer everywhere it recurs.
#[test]
fn a_reconverging_heritage_graph_that_reaches_the_target_is_assignable() {
    const WIDTH: usize = 4;
    const DEPTH: usize = 6;

    let mut source = String::from("import { Decimal } from 'money';\n");
    for level in 0..DEPTH {
        for node in 0..WIDTH {
            let parents: Vec<String> = if level + 1 == DEPTH {
                vec!["Decimal".to_owned()]
            } else {
                (0..WIDTH).map(|p| format!("N{}_{p}", level + 1)).collect()
            };
            let _ = writeln!(
                source,
                "interface N{level}_{node} extends {} {{}}",
                parents.join(", ")
            );
        }
    }
    source.push_str("let x: N0_0;\n");

    assert_eq!(
        assignable("assignable-reconverging-true", &[], &source),
        Some(true)
    );
}

/// The direct match: the symbol's module and exported name are the ones asked about.
#[test]
fn a_type_imported_from_the_named_module_is_assignable() {
    assert_eq!(
        assignable(
            "assignable-direct",
            &[],
            "import { Decimal } from 'money';\nlet x: Decimal;\n"
        ),
        Some(true)
    );
}

/// A subclass reaches it through `class_heritage` → `extends_clause` (field `value`).
#[test]
fn a_subclass_of_the_named_type_is_assignable() {
    assert_eq!(
        assignable(
            "assignable-subclass",
            &[],
            "import { Decimal } from 'money';\nclass Big extends Decimal {}\nlet x: Big;\n"
        ),
        Some(true)
    );
}

/// An interface reaches it through `extends_type_clause` (field `type`) — a *different* node
/// kind, and a walk that handles one is the obvious bug.
///
/// Several parents, because `extends_type_clause`'s `type` field is `"multiple": true` and a
/// walk reading only the first would answer `false` for the second.
#[test]
fn an_interface_extending_several_parents_reaches_the_named_type() {
    assert_eq!(
        assignable(
            "assignable-interface",
            &[],
            "import { Decimal, Amountish } from 'money';\n\
             interface Both extends Amountish, Decimal {}\n\
             let x: Both;\n"
        ),
        Some(true)
    );
}

/// An alias is transparent, on the same terms `typeOf` already follows one.
#[test]
fn an_alias_of_the_named_type_is_assignable() {
    assert_eq!(
        assignable(
            "assignable-alias",
            &[],
            "import { Money } from 'money';\nlet x: Money;\n"
        ),
        Some(true)
    );
}

/// A union is assignable only if every member is.
#[test]
fn a_union_is_assignable_only_when_every_member_is() {
    assert_eq!(
        assignable(
            "assignable-union-yes",
            &[],
            "import { Decimal, Money } from 'money';\nlet x: Decimal | Money;\n"
        ),
        Some(true)
    );
    assert_eq!(
        assignable(
            "assignable-union-no",
            &[],
            "import { Decimal } from 'money';\nlet x: Decimal | number;\n"
        ),
        Some(false)
    );
}

/// A primitive is not a nominal type, so the answer is a real `false` rather than nothing.
#[test]
fn a_primitive_is_not_assignable_to_a_nominal_type() {
    assert_eq!(
        assignable("assignable-primitive", &[], "let x: number;\n"),
        Some(false)
    );
}

/// A type the oracle could not read answers `undefined`, which a rule must not read as `false`.
///
/// Two shapes: a type with no symbol at all (an ambient global), and one whose declaration
/// file is not there. Both are "a link could not be read", and both must be distinguishable
/// from "the walk completed and found nothing".
#[test]
fn a_type_the_oracle_could_not_read_answers_nothing() {
    assert_eq!(
        assignable("assignable-ambient", &[], "let x: Date;\n"),
        None
    );
    assert_eq!(
        assignable(
            "assignable-unreadable",
            &[],
            "import { Gone } from 'not-installed';\nlet x: Gone;\n"
        ),
        None
    );
}

/// A heritage cycle terminates rather than running away.
#[test]
fn a_heritage_cycle_terminates() {
    assert_eq!(
        assignable(
            "assignable-cycle",
            &[],
            "interface A extends B {}\ninterface B extends A {}\nlet x: A;\n"
        ),
        Some(false)
    );
}

/// The second member of a union re-encounters an ancestor the first member already walked,
/// through a different path. `visited` has to be path-scoped — removed once a node's own
/// answer is known — or the second member finds the ancestor already marked "seen" by the
/// first and gets `Some(false)` for a program that really does reach the named type.
#[test]
fn a_union_whose_members_share_an_assignable_ancestor_is_assignable() {
    assert_eq!(
        assignable(
            "assignable-union-shared-class",
            &[],
            "import { Decimal } from 'money';\n\
             class Base extends Decimal {}\n\
             class Cash extends Base {}\n\
             class Coin extends Base {}\n\
             let x: Cash | Coin;\n"
        ),
        Some(true)
    );
}

/// The interface shape of the same bug: two siblings extending one shared parent, asked
/// about together in a union.
#[test]
fn a_union_of_interfaces_sharing_an_assignable_parent_is_assignable() {
    assert_eq!(
        assignable(
            "assignable-union-shared-interface",
            &[],
            "import { Decimal } from 'money';\n\
             interface Sh extends Decimal {}\n\
             interface L extends Sh {}\n\
             interface R2 extends Sh {}\n\
             let x: L | R2;\n"
        ),
        Some(true)
    );
}

/// A local class sharing the target's *name* is not the target: nominality is by declaring
/// file and export name, never by spelling.
#[test]
fn a_local_class_with_the_targets_name_is_not_the_target() {
    assert_eq!(
        assignable(
            "assignable-shadow-name",
            &[],
            "class Decimal {}\nlet x: Decimal;\n"
        ),
        Some(false)
    );
}

/// A second installed package that happens to export a same-named `Decimal` is not the
/// `money` package's `Decimal` either — the declaring *file* has to match, not only the name.
#[test]
fn the_targets_name_from_a_different_module_is_not_the_target() {
    assert_eq!(
        assignable(
            "assignable-other-module",
            &[
                (
                    "node_modules/other/package.json",
                    r#"{"types": "./index.d.ts"}"#,
                ),
                (
                    "node_modules/other/index.d.ts",
                    "export declare class Decimal {}\n",
                ),
            ],
            "import { Decimal } from 'other';\nlet x: Decimal;\n"
        ),
        Some(false)
    );
}

/// An imported alias of a primitive is a completed walk that finds nothing nominal, not an
/// unreadable link: `export type Amount = number` is `number`, and `number` is not the named
/// type — so this is `Some(false)`, distinct from the `None` an unreadable alias would give.
#[test]
fn an_imported_alias_of_a_primitive_is_not_assignable() {
    assert_eq!(
        assignable(
            "assignable-alias-primitive",
            &[],
            "import { Amount } from 'money';\nlet x: Amount;\n"
        ),
        Some(false)
    );
}

/// A declared `implements` is a nominal relationship too, on the same terms `extends` is.
#[test]
fn a_class_implementing_the_named_interface_is_assignable() {
    assert_eq!(
        assignable_target(
            "assignable-implements",
            &[],
            "import { Amountish } from 'money';\nclass C implements Amountish {}\nlet x: C;\n",
            "money",
            "Amountish"
        ),
        Some(true)
    );
}

/// Documents a real limitation rather than asserting the true answer: declarations are
/// looked up at a file's *top level* only, so `f`'s function-local `interface Wrapper` is
/// invisible to the walk and the top-level `Wrapper extends Decimal` answers in its place.
/// The honest answer for `x`'s own type is `Some(false)` — the local `Wrapper` has no
/// heritage at all — and this asserts today's `Some(true)` instead, which is the documented
/// cost of the `(file, name)` model rather than the truth. See `is_assignable_to`'s doc.
#[test]
fn a_function_local_shadow_is_a_documented_limitation() {
    assert_eq!(
        assignable(
            "assignable-local-shadow",
            &[],
            "import { Decimal } from 'money';\n\
             interface Wrapper extends Decimal {}\n\
             function f() {\n\
                 interface Wrapper { amount: number }\n\
                 let x: Wrapper;\n\
             }\n"
        ),
        Some(true)
    );
}

/// A comment written between `implements` clause members is a named child of the same
/// kind as a real one — `node-types.json` gives `implements_clause` no field for its
/// members, so a naive walk over `named_children` cannot tell a `/* x */` from an interface
/// name. `class C implements A, /* x */ B {}` must still answer `Some(true)` for `B`.
///
/// Addendum C1 (task 4.16, from Task 14's review): the `B` assertion alone proves nothing
/// about comment filtering — `heritage_of` walking the comment *unfiltered* would still find
/// `B` right after it and answer `Some(true)` regardless, since a comment node interspersed
/// between real members does not hide the member that follows it. What a broken filter would
/// actually surface is a *spurious* member: `inner_type_name` called on a `comment` node has
/// no `name` field, answers the comment node itself, and `assignable`'s walk over that would
/// find no declaration and degrade the whole answer to `None`. `D` — a third interface `C`
/// does not implement at all — is what makes that visible: filtered, the walk correctly
/// establishes `C` has no relationship to `D` and answers `Some(false)`; unfiltered, the
/// bogus comment-derived member turns that same honest "no" into "unreadable".
#[test]
fn a_comment_inside_an_implements_clause_is_skipped() {
    let fixtures = [
        (
            "node_modules/ab/package.json",
            r#"{"types": "./index.d.ts"}"#,
        ),
        (
            "node_modules/ab/index.d.ts",
            "export declare interface A { a: number }\n\
             export declare interface B { b: number }\n\
             export declare interface D { d: number }\n",
        ),
    ];
    let subject = "import { A, B } from 'ab';\nclass C implements A, /* x */ B {}\nlet x: C;\n";
    assert_eq!(
        assignable_target(
            "assignable-implements-comment",
            &fixtures,
            subject,
            "ab",
            "B"
        ),
        Some(true)
    );
    assert_eq!(
        assignable_target(
            "assignable-implements-comment-unrelated",
            &fixtures,
            subject,
            "ab",
            "D"
        ),
        Some(false),
        "C implements neither A-via-comment nor D, and a comment leaking into the walk as a \
         member would degrade this honest `false` to `None`"
    );
}

/// A file every import of which resolves is complete; one with a dead import is not.
///
/// Addendum C2 (task 4.16) widens this table with the import shapes `imports_with_names`
/// had not exercised: a default import, a namespace import, `import type`, a side-effect
/// `import 'm'` with no bound name at all, and — separately, since it needs its own
/// assertion below rather than a boolean here — a file with two imports where only one
/// misses.
#[test]
fn completeness_is_decided_by_whether_every_import_resolved() {
    for (test, fixtures, subject, expected) in [
        ("complete-none", vec![], "const x = 1;\n", true),
        (
            "complete-all",
            vec![("src/money.d.ts", "export declare const rate: number;\n")],
            "import { rate } from './money';\nconst x = rate;\n",
            true,
        ),
        (
            "complete-missing",
            vec![],
            "import { rate } from './dist/money';\nconst x = rate;\n",
            false,
        ),
        (
            "complete-default",
            vec![(
                "src/money.d.ts",
                "declare const rate: number;\nexport default rate;\n",
            )],
            "import rate from './money';\nconst x = rate;\n",
            true,
        ),
        (
            "complete-default-missing",
            vec![],
            "import rate from './dist/money';\nconst x = rate;\n",
            false,
        ),
        (
            "complete-namespace",
            vec![("src/money.d.ts", "export declare const rate: number;\n")],
            "import * as money from './money';\nconst x = money.rate;\n",
            true,
        ),
        (
            "complete-namespace-missing",
            vec![],
            "import * as money from './dist/money';\nconst x = money.rate;\n",
            false,
        ),
        (
            "complete-type",
            vec![("src/money.d.ts", "export type Amount = number;\n")],
            "import type { Amount } from './money';\nlet x: Amount;\n",
            true,
        ),
        (
            "complete-type-missing",
            vec![],
            "import type { Amount } from './dist/money';\nlet x: Amount;\n",
            false,
        ),
        (
            "complete-side-effect",
            vec![("src/setup.d.ts", "export {};\n")],
            "import './setup';\n",
            true,
        ),
        (
            "complete-side-effect-missing",
            vec![],
            "import './dist/setup';\n",
            false,
        ),
    ] {
        let project = Project::new(test, &fixtures);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let tree = parse(subject);
        let file = FilePath::new("src/a.ts");
        assert_eq!(
            provider.complete(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node: tree.root_node(),
                files: &files,
            }),
            expected,
            "{subject}"
        );
    }
}

/// A file with two imports, only one of which misses, is incomplete — and both files' probes
/// are recorded, not only the one that failed.
///
/// Addendum C2's other half: the boolean table above cannot show that a resolved import is
/// still probed (and recorded) even once an earlier import has already made the file
/// incomplete — the eager pass this docs on `complete` promises.
#[test]
fn completeness_with_two_imports_only_one_missing_records_both_probes() {
    let project = Project::new(
        "complete-two-imports-one-missing",
        &[("src/money.d.ts", "export declare const rate: number;\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { rate } from './money';\n\
                   import { gone } from './dist/absent';\n\
                   const x = rate;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(!provider.complete(Query {
        file: &file,
        tree: &tree,
        source: subject,
        node: tree.root_node(),
        files: &files,
    }));

    let reads = files.dependencies();
    assert!(
        reads
            .iter()
            .any(|read| read.path.as_str() == "src/money.ts"
                || read.path.as_str() == "src/money.d.ts"),
        "the resolving import is still probed and recorded: {reads:?}"
    );
    assert!(
        reads
            .iter()
            .any(|read| read.path.as_str() == "src/dist/absent.d.ts"),
        "the missing import is recorded too, with a null hash: {reads:?}"
    );
}

/// `import x = require('m')` is counted, even though its `source` lives on the nested
/// `import_require_clause` rather than on the `import_statement` itself.
///
/// Addendum C3. `imports_with_names` (the old `import_specifiers`) reads
/// `statement.child_by_field_name("source")` directly on the `import_statement`; for this
/// shape that field is unset — `node-types.json` marks it `required: false` on
/// `import_statement` and puts the *actual* required `source` field on
/// `import_require_clause`, its child. Missed, `import x = require('./dist/money')` would
/// count as zero imports and this file would answer complete despite depending on a module
/// that resolves to nothing.
#[test]
fn completeness_counts_an_import_equals_require_clause() {
    let project = Project::new("complete-import-require", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import money = require('./dist/money');\nconst x = money;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the only import is this one, and it resolves to nothing"
    );
}

/// And the same shape resolving is complete, which is what says the row above measures the
/// clause rather than the fixture.
///
/// Addendum C3's missing pair: `completeness_counts_an_import_equals_require_clause` asserts
/// `false` for an `import x = require('m')` that resolves to nothing — an answer a provider
/// that dropped the shape entirely would also give, since a file with no imports at all is
/// complete only when the count is right. This one resolves, so it can only be `true` if the
/// clause was both found and followed.
#[test]
fn completeness_counts_a_resolving_import_equals_require_clause() {
    let project = Project::new(
        "complete-import-require-resolving",
        &[("src/money.d.ts", "export declare const rate: number;\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import money = require('./money');\nconst x = money;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the only import is this one, and it resolves to a declaration file"
    );
    assert!(
        files
            .dependencies()
            .iter()
            .any(|read| read.path.as_str() == "src/money.d.ts"),
        "and it was really probed: {:?}",
        files.dependencies()
    );
}

/// The pass is eager: asking about completeness records every probe, absences included.
///
/// This is the half the cache rests on. A rule that stayed silent because a declaration was
/// missing must be reconsidered the moment it appears, and the only thing that can make that
/// happen is a recorded read with a null hash.
#[test]
fn deciding_completeness_records_every_probe_including_the_misses() {
    let project = Project::new("complete-records", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { rate } from './dist/money';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");

    assert!(!provider.complete(Query {
        file: &file,
        tree: &tree,
        source: subject,
        node: tree.root_node(),
        files: &files,
    }));

    let reads = files.dependencies();
    assert!(!reads.is_empty(), "an absent import records nothing");
    assert!(
        reads.iter().all(|read| read.hash().is_none()),
        "every candidate was absent, so every one is a null-hash dependency: {reads:?}"
    );
    assert!(
        reads
            .iter()
            .any(|read| read.path.as_str() == "src/dist/money.d.ts"),
        "the declaration spelling has to be among the probes: {reads:?}"
    );
}

/// A file rewritten between two accesses is re-parsed rather than served from the run cache.
///
/// The declaration cache is keyed by path alone, so a `Declaration` parsed for one importing
/// file was handed to the next one whatever its bytes now say — while that importer's own
/// `FileAccess` recorded the *new* hash. The entry written then describes neither version: it
/// carries the new bytes' hash and an answer computed from the old ones, and it validates
/// forever. Routine under `--watch`, where a file is rewritten mid-run by construction.
#[test]
fn a_declaration_rewritten_between_two_accesses_is_reparsed() {
    let project = Project::new(
        "declaration-rewritten",
        &[("lib.d.ts", "export declare class Before {}\n")],
    );
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let path = FilePath::new("lib.d.ts");

    let first = project.files();
    let before = provider.declaration(&first, &path).expect("reads");
    assert!(
        lanekeep_types::declared_here(&before, "Before").is_some(),
        "the first access sees the first version"
    );

    project.write("lib.d.ts", "export declare class After {}\n");
    // A fresh access, which is what the next file in the run gets.
    let second = project.files();
    let after = provider.declaration(&second, &path).expect("reads");
    assert!(
        lanekeep_types::declared_here(&after, "After").is_some(),
        "the second access must see what its own read hashed"
    );
    assert_ne!(before.hash, after.hash);
}

/// A stylesheet, a JSON asset and an image are not modules this provider reads.
///
/// `import './app.css'` fails every probe, so an eager completeness pass counted the file
/// incomplete and recorded eight absent reads for it — on a React codebase that is most files,
/// and the label "incomplete" then means "this project has CSS" rather than "a type answer is
/// missing". A specifier whose final segment carries an extension the resolver cannot answer
/// is not a module the oracle reads, so it is skipped entirely: no probe, no dependency, no
/// bearing on completeness.
#[test]
fn completeness_skips_a_specifier_that_is_not_code() {
    for (test, subject) in [
        ("complete-css", "import './app.css';\n"),
        ("complete-json", "import data from './x.json';\n"),
        ("complete-svg", "import logo from './logo.svg';\n"),
        ("complete-css-package", "import 'bootstrap/dist/x.css';\n"),
    ] {
        let project = Project::new(test, &[]);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let tree = parse(subject);
        let file = FilePath::new("src/a.ts");
        assert!(
            provider.complete(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node: tree.root_node(),
                files: &files,
            }),
            "{subject}"
        );
        assert!(
            files.dependencies().is_empty(),
            "nothing was probed for it: {:?}",
            files.dependencies()
        );
    }
}

/// A package whose *name* carries a dot is still a module.
///
/// The extension test is applied to a subpath, never to a bare package root: `lodash.debounce`
/// and `socket.io` are real packages, and reading `debounce` or `io` as a file extension would
/// skip them — which answers `complete()` `true` for a file whose imports were never resolved
/// at all, the exact failure the flag exists to prevent.
#[test]
fn completeness_still_counts_a_package_whose_name_has_a_dot() {
    let project = Project::new("complete-dotted-package", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { debounce } from 'lodash.debounce';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the package is not installed, so the file is incomplete"
    );
}

/// An `ERROR` covering the reached declaration makes the importer incomplete; an `ERROR`
/// in a sibling statement does not. `complete()` walks each named import to the node that
/// declares it and asks whether an `ERROR` span overlaps that node's — the granularity
/// #229 asks for, in place of the whole-file verdict.
#[test]
fn completeness_is_false_only_when_an_error_overlaps_the_reached_declaration() {
    for (test, declaration, expected) in [
        (
            // Dump-verified: the ERROR the parser recovered inside the class body
            // ([28..29]) intersects the class_declaration's own span ([15..39]).
            "complete-covered-declaration",
            "export declare class Big { m(: number }\n",
            false,
        ),
        (
            // The ERROR is a sibling statement ([28..38]); Big's declaration ([15..27])
            // is clean and reachable, so the whole-file verdict no longer poisons it.
            "complete-unrelated-error",
            "export declare class Big {}\ngarbage )(\n",
            true,
        ),
        (
            "complete-sound-declaration",
            "export declare class Big {}\n",
            true,
        ),
    ] {
        let project = Project::new(test, &[("src/big.d.ts", declaration)]);
        let files = project.files();
        let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
        let subject = "import { Big } from './big';\nlet x: Big;\n";
        let tree = parse(subject);
        let file = FilePath::new("src/a.ts");
        assert_eq!(
            provider.complete(Query {
                file: &file,
                tree: &tree,
                source: subject,
                node: tree.root_node(),
                files: &files,
            }),
            expected,
            "{declaration}"
        );
    }
}

/// A nameless import — side-effect here — has no single declaration to reach, so the
/// whole-file verdict stays for it: any ERROR anywhere in the module counts.
#[test]
fn a_nameless_import_keeps_the_whole_file_verdict() {
    let project = Project::new(
        "complete-side-effect-error",
        &[("src/big.d.ts", "export declare class Big {}\ngarbage )(\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import './big';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "a side-effect import asserts the module's shape; any ERROR counts"
    );
}

/// A mixed clause — `import d, * as ns from 'm'` — binds a module object *as well as* a
/// name, and the module object reaches everywhere: the whole-file verdict stays for the
/// statement even though its named half could have been walked.
#[test]
fn a_mixed_clause_keeps_the_whole_file_verdict() {
    let project = Project::new(
        "complete-mixed-clause-error",
        &[(
            "src/big.d.ts",
            "export declare const ok: number;\nexport default ok;\ngarbage )(\n",
        )],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    // With a default export the `Default` half is reached cleanly, so only the namespace
    // half can turn the verdict: `import ok from './big'` alone is complete here.
    assert!(complete_of(
        &project,
        "import ok from './big';\nconst y = ok;\n"
    ));
    let subject = "import ok, * as ns from './big';\nconst y = ok;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the namespace binding reaches every member; any ERROR counts"
    );
}

/// The heritage arm of `is_assignable_to` answers None — unreadable — when a parent
/// declaration the walk visited is error-covered, rather than a confident answer computed
/// over a damaged tree. The chain here is X → Mid → Damaged, asked against `Root`: the
/// walk must cross Damaged's covered node to answer, so it refuses.
///
/// The question is asked at the `type_identifier` the implements clause names — dumped:
/// both the class name and the heritage member parse as `type_identifier` in this grammar,
/// and the only plain `identifier` in the subject is the import specifier, whose type this
/// bare oracle cannot read at all. Asking there would answer None without walking
/// anything, which is the one assertion that cannot fail.
#[test]
fn an_error_covered_heritage_parent_is_unreadable_not_negative() {
    let project = Project::new(
        "heritage-error-covered",
        &[(
            "src/big.d.ts",
            "export interface Root { r(): void }\nexport interface Mid extends Damaged { m(): void }\nexport interface Damaged { d: ;;; }\n",
        )],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { Mid } from './big';\nclass X implements Mid {}\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_identifier");
    assert_eq!(
        provider.is_assignable_to(
            Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &files,
            },
            "./big",
            "Root",
        ),
        None,
        "a damaged link in the chain is unreadable, never a negative"
    );
}

/// The answer is memoized per file, so a rule asking twice costs one pass.
#[test]
fn completeness_is_decided_once_per_file() {
    let project = Project::new("complete-once", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { rate } from './dist/money';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let query = Query {
        file: &file,
        tree: &tree,
        source: subject,
        node: tree.root_node(),
        files: &files,
    };

    assert!(!provider.complete(query));
    let after_first = files.dependencies().len();
    assert!(!provider.complete(query));
    assert_eq!(
        files.dependencies().len(),
        after_first,
        "the pass ran twice"
    );
}

/// The builtin provider's `begin_run` never asks for the corpus either.
///
/// The default body is pinned above; this pins the override, which is the one an engine
/// actually calls. Answering an empty key term is only half the contract — the other half is
/// that nothing here *walks* the file list, because building the list is work every run would
/// pay for an answer this provider does not use. A closure that records being called is the
/// only way to see the difference, since both spellings return the same `Ok(Vec::new())`.
#[test]
fn the_builtin_providers_begin_run_does_not_walk_the_corpus() {
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let walked = std::cell::Cell::new(false);
    let files = || {
        walked.set(true);
        Vec::new()
    };
    assert_eq!(provider.begin_run(&files, budget()), Ok(Vec::new()));
    assert!(
        !walked.get(),
        "this provider's dependencies are the tracked reads on each entry, so there is \
         nothing to build up front"
    );
}

/// A dotted *module* — `./user.service` — is probed, because only asset extensions are skipped.
///
/// The skip was an allowlist of code extensions applied to the last dotted run, so
/// `./user.service` read as an extension `service`, matched nothing in the list, and was
/// skipped entirely — `complete()` answered `true` for a file whose imports were never
/// resolved, which is the one thing the flag exists not to say. The NestJS and Angular
/// conventions are all of this shape: `.service`, `.component`, `.module`, `.dto`, `.entity`,
/// `.guard`, `.pipe`, `.config`.
#[test]
fn completeness_probes_a_dotted_module_specifier() {
    let project = Project::new(
        "complete-dotted-module",
        &[("src/user.service.ts", "export const user = 1;\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { user } from './user.service';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "the module is there, so the file is complete"
    );
    assert!(
        files
            .dependencies()
            .iter()
            .any(|read| read.path.as_str() == "src/user.service.ts"),
        "and it was really probed rather than skipped: {:?}",
        files.dependencies()
    );
}

/// And the same specifier with nothing behind it is incomplete.
///
/// The pair the row above needs: skipping a specifier also answers `true`, so only the
/// missing case can say the probe happened at all.
#[test]
fn completeness_is_false_when_a_dotted_module_is_missing() {
    let project = Project::new("complete-dotted-module-missing", &[]);
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { user } from './user.service';\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    assert!(
        !provider.complete(Query {
            file: &file,
            tree: &tree,
            source: subject,
            node: tree.root_node(),
            files: &files,
        }),
        "nothing answers the specifier, so the file is incomplete"
    );
}

/// A subtree the depth bound cut must not memoize its `None` for a shallower reach.
///
/// The bound returns before anything is counted, so a declaration whose walk was truncated
/// wrote `None` into the per-call memo — and the *same* declaration, reached one hop from the
/// root where the bound is nowhere near, then read that `None` back instead of the `Some(true)`
/// the walk would have produced. `Root extends C1, D` is exactly that shape: the `C1` chain
/// spends the bound on the way down to `D`, and `D` is also Root's own second parent.
#[test]
fn a_subtree_cut_by_the_depth_bound_is_not_memoized() {
    let mut source =
        String::from("import { Decimal } from 'money';\ninterface D extends Decimal {}\n");
    for level in (1..=14).rev() {
        let parent = if level == 14 {
            "D".to_owned()
        } else {
            format!("C{}", level + 1)
        };
        let _ = writeln!(source, "interface C{level} extends {parent} {{}}");
    }
    source.push_str("interface Root extends C1, D {}\nlet x: Root;\n");

    assert_eq!(
        assignable("assignable-depth-memo", &[], &source),
        Some(true),
        "`D` extends the named type, and Root extends `D` directly"
    );
}

/// And the same, for a subtree the *oracle* truncated rather than the walk.
///
/// `heritage_assignable` reads a `type` alias's right-hand side with
/// [`TypeScriptOracle::type_of_from`], threading the depth the walk has already spent — and
/// that oracle has a bound of its own. When it gives up on it, the alias branch falls through
/// to the heritage loop, which for an alias has no parents at all, so the walk answered
/// `Some(false)` and memoized it as a property of the declaration. Only the walk's *own*
/// `MAX_EXPORT_DEPTH` return was counted before, so nothing knew that answer described a
/// prefix of the graph rather than the alias.
///
/// The shape is the sibling test's, with the alias chain doing the truncating. The chain has
/// to live in the *declaration file*: a same-file `extends` on an alias goes through
/// `type_named_by`, which starts a fresh depth on every call, so no local chain can ever spend
/// a depth the walk brought with it. Twelve `extends` hops reach the imported `A0`, its four
/// alias hops to `Decimal` spend what is left of the oracle's bound, and `A0` is also `Root`'s
/// own second parent — where the bound is nowhere near and the walk would answer `true`.
#[test]
fn a_subtree_the_oracle_truncated_is_not_memoized() {
    let mut declarations =
        String::from("export declare class Decimal {}\nexport type A4 = Decimal;\n");
    for level in (0..4).rev() {
        let _ = writeln!(declarations, "export type A{level} = A{};", level + 1);
    }
    let mut source = String::from("import { A0 } from 'money';\n");
    for level in (1..=12).rev() {
        let parent = if level == 12 {
            "A0".to_owned()
        } else {
            format!("C{}", level + 1)
        };
        let _ = writeln!(source, "interface C{level} extends {parent} {{}}");
    }
    source.push_str("interface Root extends C1, A0 {}\nlet x: Root;\n");

    let project = Project::new(
        "assignable-oracle-depth-memo",
        &[
            (
                "node_modules/money/package.json",
                r#"{"types": "./index.d.ts"}"#,
            ),
            ("node_modules/money/index.d.ts", &declarations),
        ],
    );
    let access = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let tree = parse(&source);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_annotation");

    assert_eq!(
        provider.is_assignable_to(
            Query {
                file: &file,
                tree: &tree,
                source: &source,
                node,
                files: &access,
            },
            "money",
            "Decimal",
        ),
        Some(true),
        "`A0` aliases the named type, and Root extends `A0` directly"
    );
}

/// One cycle must not disable the memo for every declaration above it.
///
/// The memo was guarded by a single counter of cycles cut anywhere in the call, so a mutual
/// pair at the bottom of a graph poisoned its whole ancestor chain and a re-converging graph
/// went back to `width^depth` paths. Lowlink is what scopes the poison to the declarations
/// really inside the cycle: a node memoizes when nothing under it reached back past it.
///
/// A termination line rather than a benchmark, like its sibling above: 4^12 is millions of
/// paths and forty-eight declarations, orders of magnitude either side of the bound.
#[test]
fn a_cycle_at_the_bottom_does_not_disable_the_memo_above_it() {
    const WIDTH: usize = 4;
    const DEPTH: usize = 12;

    let mut source = String::from(
        "import { Decimal } from 'money';\n\
         interface L extends L2 {}\n\
         interface L2 extends L {}\n",
    );
    for level in 0..DEPTH {
        for node in 0..WIDTH {
            let parents: Vec<String> = if level + 1 == DEPTH {
                vec!["L".to_owned()]
            } else {
                (0..WIDTH).map(|p| format!("N{}_{p}", level + 1)).collect()
            };
            let _ = writeln!(
                source,
                "interface N{level}_{node} extends {} {{}}",
                parents.join(", ")
            );
        }
    }
    source.push_str("let x: N0_0;\n");

    let started = std::time::Instant::now();
    let answer = assignable("assignable-cycle-below", &[], &source);
    let elapsed = started.elapsed();
    assert_eq!(
        answer,
        Some(false),
        "the graph is fully readable and bottoms out in a cycle rather than the named type"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "one cycle must not cost the memo for everything above it: took {elapsed:?}"
    );
}

// --- #232 review: the resolve-and-parse contract, judged by node ----------------------------

/// `complete()` for `subject` as `src/a.ts`, over the builtin provider with no tsx grammar.
fn complete_of(project: &Project, subject: &str) -> bool {
    ask(project, subject, TypeProvider::complete)
}

/// A missing token is a parse fault as much as an `ERROR` node is. `has_error()` on the
/// reached declaration sees both; a span check over recorded `ERROR` ranges saw only the
/// second, and let a class the parser never finished reading through as read.
#[test]
fn a_missing_token_in_the_reached_declaration_is_unread() {
    let project = Project::new(
        "complete-missing-token",
        &[("src/big.d.ts", "export declare class Big { m(): void\n")],
    );
    assert!(
        !complete_of(&project, "import { Big } from './big';\nconst y = Big;\n"),
        "an unclosed class body is a declaration the parser did not finish reading"
    );
    assert!(
        !complete_of(&project, "import './big';\nconst y = 1;\n"),
        "and the nameless arm agrees with the named one about the same file"
    );
}

/// A name the walk cannot model is not evidence that anything went unread: the module
/// resolved and parsed, which is the contract. `export = X` beside `declare namespace X` is
/// the shape most `@types` packages ship, and a rule gated on `complete()` must not go
/// silent on every file that names one of its members.
#[test]
fn a_name_the_walk_cannot_model_leaves_a_clean_module_complete() {
    let project = Project::new(
        "complete-export-assignment",
        &[(
            "node_modules/@types/react/index.d.ts",
            "export = React;\nexport as namespace React;\ndeclare namespace React {\n  function useState(): void;\n}\n",
        )],
    );
    assert!(complete_of(
        &project,
        "import { useState } from 'react';\nuseState();\n"
    ));
    assert!(complete_of(
        &project,
        "import React from 'react';\nReact;\n"
    ));
    assert!(complete_of(
        &project,
        "import * as React from 'react';\nReact;\n"
    ));
}

/// A barrel written as two statements — `import { A } from './a'; export { A };` — is
/// walked through its import: the local clause names an imported binding, and the walk
/// continues into the module it came from rather than stopping at a name nothing here
/// declares.
#[test]
fn a_two_statement_barrel_is_walked_through_its_import() {
    let project = Project::new(
        "barrel-two-statements",
        &[
            ("src/index.ts", "import { A } from './a';\nexport { A };\n"),
            ("src/a.ts", "export const A: number = 1;\n"),
        ],
    );
    let subject = "import { A } from './index';\nconst y = A;\n";
    assert!(complete_of(&project, subject));
    assert_eq!(
        ask(&project, subject, TypeProvider::type_of),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        )),
        "the chain continues into `./a`, so the value types from its declaration"
    );
}

/// The walk continues under the module's own spelling of the name, not the barrel's alias.
#[test]
fn a_two_statement_barrel_follows_the_import_alias() {
    let project = Project::new(
        "barrel-two-statements-alias",
        &[
            (
                "src/index.ts",
                "import { A as B } from './a';\nexport { B as C };\n",
            ),
            ("src/a.ts", "export const A: number = 1;\n"),
        ],
    );
    assert_eq!(
        ask(
            &project,
            "import { C } from './index';\nconst y = C;\n",
            TypeProvider::type_of
        ),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        ))
    );
}

/// `export * as ns from 'm'` binds a module object: nothing to walk to, and nothing unread.
#[test]
fn a_namespace_re_export_leaves_the_importer_complete() {
    let project = Project::new(
        "barrel-namespace-reexport",
        &[
            ("src/barrel.ts", "export * as utils from './utils';\n"),
            ("src/utils.ts", "export const u: number = 1;\n"),
        ],
    );
    assert!(complete_of(
        &project,
        "import { utils } from './barrel';\nconst y = utils;\n"
    ));
}

/// `namespace A.B {}` declares `A`; the dotted spelling is no binding. The name a chain
/// reports is the one the enclosing scope sees, not the text of the `nested_identifier`.
#[test]
fn a_dotted_namespace_declares_its_first_segment() {
    let project = Project::new(
        "dotted-namespace",
        &[
            (
                "src/legacy.d.ts",
                "export declare namespace A.B { const q: number }\n",
            ),
            ("src/index.ts", "export { A as Ns } from './legacy';\n"),
        ],
    );
    let subject = "import { Ns } from './index';\nconst y = Ns;\n";
    assert!(
        complete_of(&project, subject),
        "the walk ends at the namespace `A` declares"
    );
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(
        symbol.exported.as_deref(),
        Some("A"),
        "the declared name is the first segment, never `A.B`"
    );
}

/// A link the walk could not read — a re-export into a file that is absent, or into a
/// declaration the parser did not finish — is unread, and the importer says so. The walk
/// failing on a *clean* file is the other case, and it is not this one.
#[test]
fn a_damaged_link_in_a_re_export_chain_is_unread() {
    let absent = Project::new(
        "chain-absent-link",
        &[("src/index.ts", "export { A } from './nowhere';\n")],
    );
    assert!(!complete_of(
        &absent,
        "import { A } from './index';\nconst y = A;\n"
    ));
    let damaged = Project::new(
        "chain-damaged-link",
        &[
            ("src/index.ts", "export { A } from './a';\n"),
            ("src/a.d.ts", "export declare class A { m(: number }\n"),
        ],
    );
    assert!(!complete_of(
        &damaged,
        "import { A } from './index';\nconst y = A;\n"
    ));
}

/// `symbolOf` reads nothing off a declaration the parser only partly read: the walk refuses
/// the damaged node, and the symbol falls back to the import's own spelling rather than the
/// alias read out of the damaged file.
#[test]
fn symbol_of_reads_nothing_from_a_damaged_declaration() {
    let project = Project::new(
        "symbol-damaged",
        &[(
            "src/big.d.ts",
            "declare class Huge { m(: number }\nexport { Huge as Big };\n",
        )],
    );
    let symbol = ask(
        &project,
        "import { Big } from './big';\nconst y = Big;\n",
        TypeProvider::symbol_of,
    )
    .expect("the import's own spelling");
    assert_eq!(symbol.exported.as_deref(), Some("Big"));
}

/// `isAssignableTo`'s target is resolved by the same walk, so a damaged target is
/// unreadable — never a confident answer either way.
#[test]
fn a_damaged_target_is_unreadable_not_assignable() {
    let project = Project::new(
        "assignable-damaged-target",
        &[("src/big.d.ts", "export declare class Big { m(: number }\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "import { Big } from './big';\nlet b: Big;\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_identifier");
    assert_eq!(
        provider.is_assignable_to(
            Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &files,
            },
            "./big",
            "Big",
        ),
        None,
        "the target's declaration was only partly read"
    );
}

/// A damaged parent in the asking file's own tree is as unreadable as one in a declaration
/// file: the node carries its own error flag, whichever tree it sits in.
#[test]
fn a_damaged_heritage_parent_in_the_asking_file_is_unreadable() {
    let project = Project::new(
        "heritage-damaged-locally",
        &[("src/big.d.ts", "export interface Root { r(): void }\n")],
    );
    let files = project.files();
    let provider = lanekeep_types::BuiltinProvider::probe(&TypeScript).expect("TypeScript");
    let subject = "interface Damaged { d: ;;; }\ninterface Mid extends Damaged { m(): void }\nclass X implements Mid {}\n";
    let tree = parse(subject);
    let file = FilePath::new("src/a.ts");
    let node = last_of(&tree, "type_identifier");
    assert_eq!(
        provider.is_assignable_to(
            Query {
                file: &file,
                tree: &tree,
                source: subject,
                node,
                files: &files,
            },
            "./big",
            "Root",
        ),
        None,
        "the chain crosses a parent the parser only partly read"
    );
}

/// A star source that cannot be read does not hide the one after it that declares the name:
/// the walk reads past it, the way it always has, and reports the source it could not read
/// only when no source answered.
#[test]
fn a_dead_star_source_does_not_hide_a_live_one() {
    let project = Project::new(
        "barrel-dead-star",
        &[
            (
                "src/index.ts",
                "export * from './removed';\nexport * from './money';\n",
            ),
            ("src/money.ts", "export const rate: number = 1;\n"),
        ],
    );
    let subject = "import { rate } from './index';\nconst y = rate;\n";
    assert_eq!(
        ask(&project, subject, TypeProvider::type_of),
        Some(lanekeep_types::Type::Primitive(
            lanekeep_types::Primitive::Number
        )),
        "the live source answers whatever sits ahead of it"
    );
    assert!(complete_of(&project, subject));
    let nowhere = Project::new(
        "barrel-dead-star-only",
        &[("src/index.ts", "export * from './removed';\n")],
    );
    assert!(
        !complete_of(
            &nowhere,
            "import { rate } from './index';\nconst y = rate;\n"
        ),
        "with no source answering, the one that could not be read is what the verdict is about"
    );
}

/// Three segments, because the grammar spells every inner level of a dotted name as a
/// `member_expression`: the declared name is still the first segment.
#[test]
fn a_three_segment_namespace_declares_its_first_segment() {
    let project = Project::new(
        "dotted-namespace-three",
        &[
            (
                "src/legacy.d.ts",
                "export declare namespace google.maps.places { const q: number }\n",
            ),
            ("src/index.ts", "export { google as G } from './legacy';\n"),
        ],
    );
    let subject = "import { G } from './index';\nconst y = G;\n";
    assert!(complete_of(&project, subject));
    let symbol = ask(&project, subject, TypeProvider::symbol_of).expect("a symbol");
    assert_eq!(symbol.exported.as_deref(), Some("google"));
}

/// A `.tsx` file is unread by a provider with no tsx grammar whatever its parse under the
/// wrong grammar happens to say: `<Foo>bar` is a type assertion to the TypeScript grammar
/// and JSX to the TSX one, so a clean parse is not a right one.
#[test]
fn a_tsx_file_is_unread_without_a_tsx_grammar_whatever_its_parse() {
    let project = Project::new(
        "relative-tsx-clean-parse",
        &[("src/Button.tsx", "export const who: string = 'b';\n")],
    );
    let subject = "import { who } from './Button';\nlet w = who;\n";
    assert!(!complete_of(&project, subject));
    assert_eq!(ask(&project, subject, TypeProvider::type_of), None);
    assert!(
        !complete_of(&project, "import './Button';\nconst y = 1;\n"),
        "the nameless arm agrees"
    );
}
