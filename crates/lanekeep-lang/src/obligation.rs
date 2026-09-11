//! The obligation (typestate) analysis capability: a *must*-question over a control-flow
//! graph — an acquired value must reach a release on every path out of a scope.
//!
//! This crate owns only the trait and its data; the analysis is per-language (lang-js
//! today), exposed through [`crate::Language::obligation_analyzer`] exactly as binding
//! resolution is through [`crate::Language::resolver`].

use tree_sitter::{Node, Tree};

/// The scope an obligation must be discharged within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationScope {
    /// Every path out of the enclosing function, `return`/`throw` included.
    Function,
    /// Every path out of the block the acquire is in.
    Block,
    /// A value acquired anywhere in the file must have a matching-key release somewhere
    /// in the file. Discharge is existence of a matching key, not reachability — sibling
    /// functions share no control-flow graph. Requires a `@key` capture.
    Module,
}

impl ObligationScope {
    /// Parse a scope name to its variant.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "function" => Some(Self::Function),
            "block" => Some(Self::Block),
            "module" => Some(Self::Module),
            _ => None,
        }
    }
}

/// An acquire or release node paired with its optional `@key` capture, for correlation.
#[derive(Debug, Clone, Copy)]
pub struct Keyed<'t> {
    /// The `@acquire` or `@release` node itself.
    pub node: Node<'t>,
    /// The `@key` node bound in the same match, when the rule's query bound one.
    pub key: Option<Node<'t>>,
}

/// An acquire that some path leaves undischarged.
#[derive(Debug, Clone)]
pub struct UnmetObligation<'t> {
    /// The acquire node that was not discharged on some path.
    pub acquire: Node<'t>,
    /// The exit the value escapes through — a `return`, a `throw`, or the implicit end.
    pub exit: Node<'t>,
    /// Whether any path *did* discharge it.
    pub partial: bool,
    /// The acquire's `@key` node, when the rule bound one — so `checkObligation` can name
    /// the value. `None` for an un-keyed obligation.
    pub key: Option<Node<'t>>,
}

/// A per-language typestate analysis over acquire/release node sets.
pub trait ObligationAnalyzer: Send + Sync {
    /// Return one [`UnmetObligation`] per acquire some path leaves undischarged, in source
    /// order of the acquire node.
    fn analyze<'t>(
        &self,
        tree: &'t Tree,
        source: &str,
        scope: ObligationScope,
        keyed: bool,
        acquires: &[Keyed<'t>],
        releases: &[Keyed<'t>],
    ) -> Vec<UnmetObligation<'t>>;
}

#[cfg(test)]
mod tests {
    use super::ObligationScope;

    #[test]
    fn scope_parses_the_three_names_and_nothing_else() {
        assert_eq!(
            ObligationScope::parse("function"),
            Some(ObligationScope::Function)
        );
        assert_eq!(
            ObligationScope::parse("block"),
            Some(ObligationScope::Block)
        );
        assert_eq!(
            ObligationScope::parse("module"),
            Some(ObligationScope::Module)
        );
        assert_eq!(ObligationScope::parse("loop"), None);
    }
}
