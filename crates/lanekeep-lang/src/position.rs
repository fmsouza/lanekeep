//! The one conversion from a tree-sitter point to a position lanekeep reports.
//!
//! One-based on both axes, the way `lanekeep_core::Position` stores them. A rule's `ctx.report`
//! reaches it through `NodeArena::position`, and the engine's `lanekeep/parse` diagnostic calls
//! it directly, so the two cannot disagree about where a node is. The column is tree-sitter's,
//! which counts bytes while `Position` documents characters; that is a known disagreement being
//! fixed separately. Every reported violation position goes through here; `lanekeep-js`'s
//! strip-error positions have their own conversion, which the same fix must also cover.

use tree_sitter::Point;

/// One-based line and column of `point`, saturating rather than wrapping for a file too large
/// for `u32`.
#[must_use]
pub fn one_based(point: Point) -> (u32, u32) {
    (
        u32::try_from(point.row)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
        u32::try_from(point.column)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_becomes_one_based() {
        assert_eq!(one_based(Point { row: 0, column: 0 }), (1, 1));
        assert_eq!(one_based(Point { row: 2, column: 8 }), (3, 9));
    }
}
