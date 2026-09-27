//! Where a parse went wrong.
//!
//! tree-sitter always returns a tree. A source it could not read comes back with `ERROR` nodes
//! where recovery skipped or regrouped tokens, and zero-width `MISSING` nodes where it invented
//! one. When recovery cannot fit the file under the grammar's start symbol at all, the root
//! itself is `ERROR`, and a query anchored at the root matches nothing. None of that is an
//! error return: a caller that does not look sees a tree like any other.
//!
//! This module is the one place that looks. Two walkers would drift into disagreeing about
//! what counts as a fault.

use tree_sitter::{Node, Point};

use crate::position::one_based;

/// The fault regions of a tree, in document order.
///
/// A region is an `ERROR` or `MISSING` node that is not inside another `ERROR`: the outermost
/// extent of each place recovery took over. Only children that carry a fault are entered, so a
/// clean subtree costs one flag read.
///
/// Iterative rather than recursive: a checked file is untrusted input, and a fault at the
/// bottom of a deeply nested expression would otherwise spend one stack frame per level on a
/// worker thread's stack.
#[must_use]
pub fn regions(root: Node<'_>) -> Vec<Node<'_>> {
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.is_error() || node.is_missing() {
            found.push(node);
        } else if node.has_error() {
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
            // Reversed, so the stack hands them back in document order.
            pending.extend(children.into_iter().rev());
        }
    }
    found
}

/// What a caller reporting a faulted tree needs, detached from the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// The root itself is `ERROR`: recovery could not fit the file under the grammar's start
    /// symbol, so no query anchored at the root can match.
    pub root_is_error: bool,
    /// One-based line where the first region's code begins.
    pub line: u32,
    /// One-based column there, through [`one_based`].
    pub column: u32,
    /// One-based last line of the first region.
    pub last_line: u32,
    /// How many regions the tree has, the first included.
    pub regions: usize,
}

/// Summarize a tree's faults, or `None` for a tree the parser read whole.
///
/// The position is where the first region's code begins: its first child that is not an extra
/// (a comment), or the region itself when it has none. For an `ERROR` root that is the file's
/// first code token, which a next-line directive can precede. The root's own start could not
/// be preceded, because a leading comment is the root's first child.
#[must_use]
pub fn summarize(root: Node<'_>) -> Option<Summary> {
    if !root.has_error() {
        return None;
    }
    let all = regions(root);
    let first = *all.first()?;
    let begins = code_start(first);
    let (line, column) = one_based(begins);
    Some(Summary {
        root_is_error: root.is_error(),
        line,
        column,
        last_line: last_line(begins, first.end_position()),
        regions: all.len(),
    })
}

/// Where a region's code begins: its first child that is not an extra, or its own start.
fn code_start(region: Node<'_>) -> Point {
    let mut cursor = region.walk();
    let first_code = region.children(&mut cursor).find(|child| !child.is_extra());
    first_code.map_or_else(|| region.start_position(), |child| child.start_position())
}

/// The one-based last line of a region running from `start` to `end`.
///
/// A region whose end falls at column 0 of a later line stopped at a newline, so its last line
/// is the one before.
fn last_line(start: Point, end: Point) -> u32 {
    let row = if end.column == 0 && end.row > start.row {
        end.row.saturating_sub(1)
    } else {
        end.row
    };
    u32::try_from(row).unwrap_or(u32::MAX).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("grammar loads");
        parser.parse(source, None).expect("parser returns a tree")
    }

    fn summary_of(source: &str) -> Option<Summary> {
        summarize(tree(source).root_node())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "every call site compares a faulted fixture against `summary_of`, which \
                  returns `Option<Summary>`; wrapping once here beats `Some(...)` at each one"
    )]
    fn at(
        root_is_error: bool,
        line: u32,
        column: u32,
        last_line: u32,
        regions: usize,
    ) -> Option<Summary> {
        Some(Summary {
            root_is_error,
            line,
            column,
            last_line,
            regions,
        })
    }

    /// The Vitest `importOriginal` idiom tree-sitter-typescript 0.23.2 misreads
    /// (tree-sitter/tree-sitter-typescript#367). With a statement after it, the root itself is
    /// `ERROR`.
    const REPRO: &str = "hoist('a', async importOriginal => {\n    const actual =\n        \
                         await importOriginal<typeof import('vitest')>()\n})\n\n1\n";

    /// The same without the trailing statement: the root survives, the declaration does not.
    const NO_TRAILING: &str = "hoist('a', async importOriginal => {\n    const actual =\n        \
                               await importOriginal<typeof import('vitest')>()\n})\n";

    /// A realistic test file: everything after the mock is lost, under a `program` root.
    const REALISTIC: &str = "import { describe, expect, it, vi } from 'vitest'\n\
                             import { fetchUser } from './api'\n\
                             \n\
                             vi.mock('./api', async importOriginal => {\n    \
                             const actual = await importOriginal<typeof import('./api')>()\n    \
                             return { ...actual, fetchUser: vi.fn() }\n\
                             })\n\
                             \n\
                             describe('fetchUser', () => {\n    \
                             it('works', async () => {\n        \
                             expect(await fetchUser(1)).toBeUndefined()\n    \
                             })\n\
                             })\n";

    #[test]
    fn a_clean_tree_has_no_faults() {
        assert_eq!(summary_of("const a = 1;\n"), None);
        assert_eq!(
            summary_of(&REPRO.replace("<typeof import('vitest')>", "<number>")),
            None,
            "the same shape with a plain type argument parses clean"
        );
    }

    #[test]
    fn an_error_root_is_anchored_at_the_first_code_token() {
        assert_eq!(summary_of(REPRO), at(true, 1, 1, 6, 1));
    }

    #[test]
    fn a_leading_comment_does_not_hold_the_anchor() {
        // The comment is the `ERROR` root's first child. A next-line directive can only cover
        // the line the code starts on, so that is where the anchor has to land.
        let source = format!("// a leading comment\n\n{REPRO}");
        assert_eq!(summary_of(&source), at(true, 3, 1, 8, 1));
    }

    #[test]
    fn without_the_trailing_statement_the_fault_is_nested() {
        assert_eq!(summary_of(NO_TRAILING), at(false, 2, 5, 3, 1));
    }

    #[test]
    fn the_realistic_mock_loses_the_rest_of_the_file() {
        assert_eq!(summary_of(REALISTIC), at(false, 4, 24, 13, 1));
    }

    #[test]
    fn a_region_on_one_line_spans_one_line() {
        assert_eq!(summary_of("const x = ;\n"), at(false, 1, 9, 1, 1));
    }

    #[test]
    fn a_missing_token_is_a_region() {
        assert_eq!(
            summary_of("export declare class Big { m(): void\n"),
            at(false, 1, 37, 1, 1)
        );
    }

    #[test]
    fn every_region_is_counted_and_the_first_is_reported() {
        assert_eq!(
            summary_of("let a = 1\nconst x = ;\nlet b = 2\nlet c = )\n"),
            at(false, 2, 9, 2, 2)
        );
        assert_eq!(
            summary_of("const x = ;\nconst y = ;\nconst z = ;\n"),
            at(false, 1, 9, 1, 3)
        );
    }

    #[test]
    fn two_regions_can_share_a_line() {
        assert_eq!(
            summary_of("const x = ; const y = ;\n"),
            at(false, 1, 9, 1, 2)
        );
    }

    #[test]
    fn regions_come_back_in_document_order() {
        let tree = tree("let a = 1\nconst x = ;\nlet b = 2\nlet c = )\n");
        let starts: Vec<Point> = regions(tree.root_node())
            .iter()
            .map(Node::start_position)
            .collect();
        assert_eq!(
            starts,
            [Point { row: 1, column: 8 }, Point { row: 3, column: 0 },]
        );
    }

    #[test]
    fn crlf_line_endings_count_lines_the_same() {
        assert_eq!(
            summary_of("let a = 1\r\nconst x = ;\r\nlet b = 2\r\n"),
            at(false, 2, 9, 2, 1)
        );
    }

    #[test]
    fn a_region_ending_at_column_zero_ends_on_the_line_before() {
        use tree_sitter::Point;
        // No fixture measured so far ends a nested region at column 0, so the rule is pinned
        // on the function itself.
        assert_eq!(
            last_line(Point { row: 2, column: 4 }, Point { row: 5, column: 0 }),
            5
        );
        assert_eq!(
            last_line(Point { row: 0, column: 8 }, Point { row: 0, column: 9 }),
            1
        );
        // A zero-width region at the start of a line is on that line.
        assert_eq!(
            last_line(Point { row: 3, column: 0 }, Point { row: 3, column: 0 }),
            4
        );
    }

    #[test]
    fn a_deeply_nested_fault_does_not_exhaust_the_stack() {
        // Measured: a recursive walk overflows a 256 KiB stack below 500 levels. This tree's
        // path to its fault is 2,004 levels deep. A checked file is untrusted input.
        const DEPTH: usize = 2_000;
        let source = format!("const v = {}x +{};\n", "(".repeat(DEPTH), ")".repeat(DEPTH));
        let column =
            u32::try_from("const v = ".len() + DEPTH + "x +".len() + 1).expect("fits in u32");
        let found = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || summary_of(&source))
            .expect("spawns")
            .join()
            .expect("the walk finished without exhausting the stack");
        assert_eq!(found, at(false, 1, column, 1, 1));
    }
}
