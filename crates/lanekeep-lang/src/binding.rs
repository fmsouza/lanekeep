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
    /// `catch (e)`
    CatchParam,
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
}
