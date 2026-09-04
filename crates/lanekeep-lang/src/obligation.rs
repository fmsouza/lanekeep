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
}

impl ObligationScope {
    /// Parse a scope name to its variant.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "function" => Some(Self::Function),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
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
        acquires: &[Node<'t>],
        releases: &[Node<'t>],
    ) -> Vec<UnmetObligation<'t>>;
}

#[cfg(test)]
mod tests {
    use super::ObligationScope;

    #[test]
    fn scope_parses_the_two_names_and_nothing_else() {
        assert_eq!(
            ObligationScope::parse("function"),
            Some(ObligationScope::Function)
        );
        assert_eq!(
            ObligationScope::parse("block"),
            Some(ObligationScope::Block)
        );
        assert_eq!(ObligationScope::parse("loop"), None);
    }
}
