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
