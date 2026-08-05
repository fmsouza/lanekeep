//! What an identifier refers to.
//!
//! This is the "light binding resolution" of architecture §1 — deliberately not type-aware
//! analysis. It answers one question: given an identifier, where does the name come from?
//!
//! That question is what pure syntactic matching gets wrong. A rule looking for
//! `makeStyles(...)` by matching the identifier text is wrong twice over: it misses
//! `import { makeStyles as ms }` and it fires on a local `const makeStyles = ...` that has
//! nothing to do with the import. Both are ordinary things to write, and both produce
//! results a user reads as the tool being broken.

use tree_sitter::{Node, Tree};

/// Which export of a module a name came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedName {
    /// `import d from 'm'`
    Default,
    /// `import * as ns from 'm'`
    Namespace,
    /// `import { a } from 'm'`, or `import { a as b }` where this is `a`.
    Named(String),
}

/// How a local name was introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// `const x = ...`
    Const,
    /// `let x = ...`
    Let,
    /// `var x = ...`
    Var,
    /// A function or method parameter.
    Param,
    /// `function x() {}`
    Function,
    /// `class X {}`
    Class,
    /// `catch (e)`, and Python's `except E as e`.
    CatchParam,

    // --- forms with no JavaScript equivalent -------------------------------------------
    //
    // Python binds by assigning, and has no keyword to distinguish `const` from `let`. The
    // kinds below say *how* a name came to be bound, which is the question a rule can act
    // on — reusing `var` for all of them would answer it with something untrue.
    /// `x = 1`, an augmented assignment, or a walrus `x := 1`.
    Assignment,
    /// The target of a `for` statement or a `for` clause.
    Loop,
    /// `with open(p) as f`.
    ContextManager,
    /// A comprehension's own target, which is scoped to the comprehension.
    Comprehension,

    // --- Go ------------------------------------------------------------------------------
    //
    // Same reasoning as above, one language further out. Go names things JavaScript has no
    // word for, and the nearest existing kind would be a lie in each case: a struct is not a
    // `class`, a method receiver is not quite a `param`, and a type parameter is neither.
    /// `type T struct{}`, `type T interface{}`, or a `type T = U` alias.
    Type,
    /// The receiver of a method — the `r` in `func (r *Repo) Get()`.
    Receiver,
    /// A generic type parameter — the `T` in `func F[T any]()` or `type S[T any] struct{}`.
    TypeParam,

    // --- Rust ----------------------------------------------------------------------------
    //
    // Two more, for the same reason as every kind above: the nearest existing one would be a
    // lie. A module is not a type — it names a namespace, not something a value can be. And
    // a trait is not a struct; reusing `type` for both would stop a rule asking the one
    // question it most wants to ask about a trait, which is whether it is one.
    /// `mod parser;` or `mod parser { ... }`.
    Module,
    /// `trait Store { ... }`.
    Trait,
}

impl BindingKind {
    /// The keyword or role, as it appears in a rule's `bindingKind` check.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Const => "const",
            Self::Let => "let",
            Self::Var => "var",
            Self::Param => "param",
            Self::Function => "function",
            Self::Class => "class",
            Self::CatchParam => "catch-param",
            Self::Assignment => "assignment",
            Self::Loop => "loop",
            Self::ContextManager => "context-manager",
            Self::Comprehension => "comprehension",
            Self::Type => "type",
            Self::Receiver => "receiver",
            Self::TypeParam => "type-param",
            Self::Module => "module",
            Self::Trait => "trait",
        }
    }
}

/// What an identifier resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// The name came from an import.
    Import {
        /// The module specifier, exactly as written.
        module: String,
        /// Which export.
        name: ImportedName,
    },
    /// The name was declared in this file.
    Local(BindingKind),
}

impl Binding {
    /// The kind as a rule sees it, with imports reported as `import`.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Import { .. } => "import",
            Self::Local(kind) => kind.as_str(),
        }
    }

    /// Whether this is the import a rule asked about — `module` exactly, and the export
    /// `name` names when it names one.
    ///
    /// `name` is spelled as a rule writes it: a named export by its own name, `default` for
    /// a default import, `*` for a namespace import. `None` matches any export of the
    /// module, which is the question "did this come from there at all".
    ///
    /// Here rather than in an engine because both engines ask it. `lanekeep-js` installs it
    /// as `ctx.resolvesToImport` and `lanekeep-wasm` implements
    /// `check-context.resolves-to-import` with it, and a copy in each would let one file
    /// resolve differently depending on which engine ran the rule — the drift sharing
    /// `NodeArena` between them exists to prevent, one layer up.
    #[must_use]
    pub fn is_import_of(&self, module: &str, name: Option<&str>) -> bool {
        let Self::Import {
            module: from,
            name: imported,
        } = self
        else {
            return false;
        };

        from.as_str() == module
            && name.is_none_or(|wanted| match imported {
                ImportedName::Named(actual) => actual.as_str() == wanted,
                ImportedName::Default => wanted == "default",
                ImportedName::Namespace => wanted == "*",
            })
    }

    /// Whether this is an import from a module matching `pattern`, where `*` stands for any
    /// run of characters.
    ///
    /// Shared for the same reason [`Binding::is_import_of`] is: `ctx.isImportedFrom` and
    /// `check-context.is-imported-from` are the same question, so they are one answer.
    #[must_use]
    pub fn is_imported_from(&self, pattern: &str) -> bool {
        match self {
            Self::Import { module, .. } => glob_matches(pattern, module),
            Self::Local(_) => false,
        }
    }
}

/// Match a module specifier against a pattern where `*` stands for any run of characters.
///
/// Written out rather than pulled in, because the whole need is `@scope/*` and `*/themed`.
/// A glob crate would bring a dependency and a dialect — character classes, `**`, escapes —
/// for a surface this small.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return true;
    };
    if !text.starts_with(first) {
        return false;
    }

    let mut rest = &text[first.len()..];
    let segments: Vec<&str> = parts.collect();

    // No `*` at all: the pattern has to account for the whole specifier.
    if segments.is_empty() {
        return rest.is_empty();
    }

    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        // The final segment has to sit at the end, or `@scope/*` would match
        // `@scope/pkg/nested` on a pattern the author meant to be exact after the star.
        if index == segments.len() - 1 {
            return rest.ends_with(segment);
        }
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }

    // The pattern ended with `*`, so whatever is left is matched.
    true
}

/// Language-specific resolution of identifiers to bindings.
///
/// Implementations are expected to be cheap to call repeatedly for one file — the engine
/// resolves per match, and a query can match many times. Building a per-file index once
/// and reusing it is the intended shape.
pub trait BindingResolver: Send + Sync {
    /// What the identifier at `node` refers to, or `None` if it is not an identifier or
    /// nothing in this file declares it.
    fn resolve(&self, tree: &Tree, source: &str, node: Node<'_>) -> Option<Binding>;

    /// Whether the identifier resolves to a binding that shadows an outer one of the same
    /// name.
    ///
    /// Distinct from `resolve` returning a local binding: a name declared once, locally, is
    /// not shadowing anything. This is specifically "there is more than one, and you got
    /// the inner one".
    fn is_shadowed(&self, tree: &Tree, source: &str, node: Node<'_>) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_kinds_render_as_rules_write_them() {
        assert_eq!(BindingKind::Const.as_str(), "const");
        assert_eq!(BindingKind::Param.as_str(), "param");
        assert_eq!(BindingKind::CatchParam.as_str(), "catch-param");
    }

    #[test]
    fn imports_report_as_import_regardless_of_which_export() {
        for name in [
            ImportedName::Default,
            ImportedName::Namespace,
            ImportedName::Named("a".to_owned()),
        ] {
            let binding = Binding::Import {
                module: "m".to_owned(),
                name,
            };
            assert_eq!(binding.kind_str(), "import");
        }
    }

    #[test]
    fn local_bindings_report_their_own_kind() {
        assert_eq!(Binding::Local(BindingKind::Const).kind_str(), "const");
        assert_eq!(Binding::Local(BindingKind::Function).kind_str(), "function");
    }

    /// The import a rule most often asks about: one named export of one module.
    fn named_import(module: &str, name: &str) -> Binding {
        Binding::Import {
            module: module.to_owned(),
            name: ImportedName::Named(name.to_owned()),
        }
    }

    #[test]
    fn an_import_matches_its_own_module_and_export() {
        let binding = named_import("@rneui/themed", "makeStyles");

        assert!(binding.is_import_of("@rneui/themed", Some("makeStyles")));
        assert!(!binding.is_import_of("somewhere-else", Some("makeStyles")));
        assert!(!binding.is_import_of("@rneui/themed", Some("notThatOne")));
    }

    #[test]
    fn omitting_the_name_matches_any_export_of_the_module() {
        let binding = named_import("m", "a");

        assert!(binding.is_import_of("m", None));
        assert!(!binding.is_import_of("other", None));
    }

    #[test]
    fn the_default_and_namespace_forms_are_named_as_a_rule_writes_them() {
        // `default` and `*` are the spellings `packages/lanekeep/index.d.ts` documents, and
        // they are what a rule has to write — there is no other way to name those imports.
        let default = Binding::Import {
            module: "m".to_owned(),
            name: ImportedName::Default,
        };
        assert!(default.is_import_of("m", Some("default")));
        assert!(!default.is_import_of("m", Some("*")));
        assert!(!default.is_import_of("m", Some("a")));

        let namespace = Binding::Import {
            module: "m".to_owned(),
            name: ImportedName::Namespace,
        };
        assert!(namespace.is_import_of("m", Some("*")));
        assert!(!namespace.is_import_of("m", Some("default")));

        // A named export called `default` is `import { default as d }`, which is the same
        // export the default form names — so both answering `true` is correct rather than a
        // collision.
        assert!(named_import("m", "default").is_import_of("m", Some("default")));
    }

    #[test]
    fn a_local_binding_is_no_import_at_all() {
        let local = Binding::Local(BindingKind::Const);

        assert!(!local.is_import_of("m", None));
        assert!(!local.is_imported_from("*"));
    }

    #[test]
    fn glob_matching_handles_the_shapes_that_appear_in_rules() {
        assert!(glob_matches("m", "m"));
        assert!(!glob_matches("m", "mm"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("@scope/*", "@scope/pkg"));
        assert!(!glob_matches("@scope/*", "@other/pkg"));
        assert!(glob_matches("*/themed", "@rneui/themed"));
        assert!(!glob_matches("*/themed", "@rneui/other"));
        assert!(glob_matches("@a/*/c", "@a/b/c"));
        assert!(!glob_matches("@a/*/c", "@a/b/d"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "x"));
    }

    #[test]
    fn an_import_is_matched_by_a_glob_over_its_module() {
        let binding = named_import("@scope/pkg", "a");

        assert!(binding.is_imported_from("@scope/*"));
        assert!(binding.is_imported_from("*/pkg"));
        assert!(binding.is_imported_from("@scope/pkg"));
        assert!(!binding.is_imported_from("@other/*"));
    }
}
