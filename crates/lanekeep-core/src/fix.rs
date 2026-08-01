//! Fixes: a replacement a rule offers for what it reported.
//!
//! A fix is a byte range and the text to put there. Template-based replacement of a capture
//! is the whole model — not a general edit script, not a patch format. A rule that matched a
//! node knows that node's extent, and replacing it is the operation that covers almost every
//! automatic fix worth having.
//!
//! # Safe and suggested
//!
//! A fix is either **safe** — applying it preserves what the code does — or a **suggestion**,
//! which is a good idea a human should look at. `--fix` applies only the safe ones.
//!
//! The distinction is the rule author's to make and it is not checkable, which is exactly
//! why the default is the cautious one: a rule that forgets to say gets a suggestion, and a
//! suggestion that should have been safe costs a manual edit. The other default would let a
//! forgotten flag silently rewrite someone's code.
//!
//! # Overlaps
//!
//! Two fixes that touch the same bytes cannot both be applied — the second would be editing
//! text the first replaced, and the result is whatever the ordering happened to be. Applying
//! one and skipping the other is the only sound choice; see [`apply`].

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// A replacement for a range of a file's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    /// Byte offset the replacement starts at.
    pub start: usize,
    /// Byte offset it ends at, exclusive.
    pub end: usize,
    /// What to put there.
    pub replacement: String,
    /// Whether applying it preserves behavior.
    ///
    /// `false` — a suggestion — is the default a rule gets by not saying, because the
    /// cautious mistake costs a manual edit and the other one rewrites code silently.
    pub safe: bool,
}

impl Fix {
    /// The range this replaces.
    #[must_use]
    pub const fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    /// Whether two fixes touch the same bytes.
    ///
    /// Adjacent is not overlapping: one ending where the next begins is two edits to
    /// different text, and both can be applied.
    #[must_use]
    pub const fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Whether this fix names a range that exists in a file of `len` bytes.
    #[must_use]
    pub const fn fits(&self, len: usize) -> bool {
        self.start <= self.end && self.end <= len
    }
}

/// What applying a set of fixes to one file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixOutcome {
    /// The file's new contents.
    pub source: String,
    /// How many fixes were applied.
    pub applied: usize,
    /// How many were skipped because another fix had already claimed those bytes.
    ///
    /// Reported rather than swallowed: a run that fixed three of five things and said it
    /// fixed everything would leave someone believing the file was clean.
    pub skipped: usize,
}

/// Apply fixes to a file's source.
///
/// Only safe fixes, only ranges that fit, and only one of any overlapping group.
///
/// Fixes are applied **last first**, so an earlier fix's offsets stay valid while later ones
/// are still being written. Applying in forward order would require adjusting every
/// subsequent offset by the length delta of every edit before it, which is the same
/// computation with more chances to get it wrong.
///
/// Where two fixes overlap, the one starting earlier wins. Arbitrary, but it has to be
/// *decided* rather than left to whatever order the rules happened to run in — two runs over
/// identical input must produce identical output.
#[must_use]
pub fn apply(source: &str, fixes: &[Fix]) -> FixOutcome {
    let mut candidates: Vec<&Fix> = fixes
        .iter()
        .filter(|fix| fix.safe && fix.fits(source.len()))
        .collect();

    // Sorted by start, then by end, so the choice among overlapping fixes does not depend on
    // the order rules ran in. Length breaks a tie because a shorter replacement at the same
    // start is the more conservative edit.
    candidates.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.end.cmp(&b.end)));

    let mut chosen: Vec<&Fix> = Vec::with_capacity(candidates.len());
    let mut skipped = 0usize;
    for fix in candidates {
        if chosen.last().is_some_and(|last| last.overlaps(fix)) {
            skipped += 1;
            continue;
        }
        chosen.push(fix);
    }

    let mut source = source.to_owned();
    let applied = chosen.len();
    for fix in chosen.iter().rev() {
        // Guarded because a range can name a byte inside a multi-byte character, which
        // `String::replace_range` would panic on. Silently declining is right: the fix was
        // wrong, and a checker must not abort over a rule's bad arithmetic.
        if source.is_char_boundary(fix.start) && source.is_char_boundary(fix.end) {
            source.replace_range(fix.range(), &fix.replacement);
        }
    }

    FixOutcome {
        source,
        applied,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(start: usize, end: usize, replacement: &str) -> Fix {
        Fix {
            start,
            end,
            replacement: replacement.to_owned(),
            safe: true,
        }
    }

    fn suggestion(start: usize, end: usize, replacement: &str) -> Fix {
        Fix {
            safe: false,
            ..fix(start, end, replacement)
        }
    }

    #[test]
    fn a_single_fix_replaces_its_range() {
        let result = apply("const a = 1;", &[fix(0, 5, "let")]);
        assert_eq!(result.source, "let a = 1;");
        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn several_fixes_all_land_in_the_right_places() {
        // The reason edits are applied last first: every earlier offset stays valid.
        let result = apply(
            "aaa bbb ccc",
            &[fix(0, 3, "xxxx"), fix(4, 7, "y"), fix(8, 11, "zzzzz")],
        );
        assert_eq!(result.source, "xxxx y zzzzz");
        assert_eq!(result.applied, 3);
    }

    #[test]
    fn a_suggestion_is_not_applied() {
        // A rule that did not say its fix preserves behavior does not get to rewrite code.
        let result = apply("const a = 1;", &[suggestion(0, 5, "let")]);
        assert_eq!(result.source, "const a = 1;");
        assert_eq!(result.applied, 0);
    }

    #[test]
    fn overlapping_fixes_are_skipped_and_counted() {
        // A run that fixed some of them and said it fixed everything would leave someone
        // believing the file was clean.
        let result = apply("aaaa", &[fix(0, 3, "x"), fix(1, 4, "y")]);
        assert_eq!(result.applied, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.source, "xa");
    }

    #[test]
    fn adjacent_fixes_are_both_applied() {
        // One ending where the next begins is two edits to different text.
        let result = apply("abcd", &[fix(0, 2, "X"), fix(2, 4, "Y")]);
        assert_eq!(result.applied, 2);
        assert_eq!(result.source, "XY");
    }

    #[test]
    fn which_of_two_overlapping_fixes_wins_does_not_depend_on_order() {
        // Two runs over identical input must produce identical output, and rules do not run
        // in a guaranteed order.
        let one = apply("aaaa", &[fix(0, 3, "x"), fix(1, 4, "y")]);
        let other = apply("aaaa", &[fix(1, 4, "y"), fix(0, 3, "x")]);
        assert_eq!(one, other);
    }

    #[test]
    fn a_range_past_the_end_is_declined() {
        // A rule's arithmetic must not be able to abort a checker.
        let result = apply("short", &[fix(0, 500, "x")]);
        assert_eq!(result.source, "short");
        assert_eq!(result.applied, 0);
    }

    #[test]
    fn an_inverted_range_is_declined() {
        let result = apply("const a = 1;", &[fix(5, 2, "x")]);
        assert_eq!(result.source, "const a = 1;");
        assert_eq!(result.applied, 0);
    }

    #[test]
    fn a_range_splitting_a_character_is_declined_rather_than_panicking() {
        // `→` is three bytes. Replacing one of them would panic in `replace_range`.
        let source = "a → b";
        let result = apply(source, &[fix(2, 3, "x")]);
        assert_eq!(result.source, source);
    }

    #[test]
    fn a_multi_byte_range_on_its_boundaries_is_applied() {
        let source = "a → b";
        let arrow = source.find('→').expect("present");
        let result = apply(source, &[fix(arrow, arrow + '→'.len_utf8(), "->")]);
        assert_eq!(result.source, "a -> b");
    }

    #[test]
    fn an_empty_replacement_deletes() {
        let result = apply("const a = 1;\n", &[fix(0, 13, "")]);
        assert_eq!(result.source, "");
        assert_eq!(result.applied, 1);
    }

    #[test]
    fn an_empty_range_inserts() {
        let result = apply("ab", &[fix(1, 1, "X")]);
        assert_eq!(result.source, "aXb");
    }

    #[test]
    fn no_fixes_leaves_the_source_alone() {
        let result = apply("const a = 1;", &[]);
        assert_eq!(result.source, "const a = 1;");
        assert_eq!(result.applied, 0);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn a_suggestion_overlapping_a_safe_fix_does_not_block_it() {
        // Suggestions are filtered before overlaps are considered, so an unapplied one
        // cannot consume the bytes a safe fix needs.
        let result = apply("aaaa", &[suggestion(0, 4, "z"), fix(0, 2, "X")]);
        assert_eq!(result.source, "Xaa");
        assert_eq!(result.applied, 1);
        assert_eq!(
            result.skipped, 0,
            "a suggestion should not count as skipped"
        );
    }
}
