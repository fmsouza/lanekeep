package lanekeep

import "strings"

// isLineTerminator reports whether c is a line terminator under ECMAScript's definition: the
// four characters a `RegExp`'s `.` does not match without the `s`/`dotAll` flag, which is how
// the TypeScript rules this was ported from built theirs. See [GlobMatches].
func isLineTerminator(c rune) bool {
	return c == '\n' || c == '\r' || c == '\u2028' || c == '\u2029'
}

// GlobMatches reports whether value matches the `*`-wildcard pattern, anchored at both ends.
//
// A port of `glob_matches` in `rust-rules/lanekeep-rule/src/lib.rs`, which is itself a port of
// the `matches` helper that was duplicated in `crates/lanekeep-rules/rules/no-unwrap.ts` and
// `no-glob-import.ts` — both deleted when their components became the shipped rules, and
// recoverable with `git log --diff-filter=D -- <path>`. That original builds a `RegExp`,
// escaping every metacharacter except `*` and then substituting `*` for `.*` — so everything
// but `*` is a literal, and (carrying no `s`/`dotAll` flag) `*` does not span a line
// terminator.
//
// This is the same rule without a regex engine: pattern is peeled into literal segments
// around its `*`s, and value matches when it starts with the first segment, ends with the
// last, and contains whatever segments sit between them, in that order, with no line
// terminator anywhere in a gap a `*` had to span. A pattern with no `*` at all has nothing to
// span, so "anchored at both ends" degenerates to an exact match.
//
// Written with [strings.Cut], [strings.CutPrefix] and [strings.CutSuffix] rather than manual
// index arithmetic, so there is no slicing here that could go out of range on a boundary this
// function got wrong. The three semantics that must not drift from the Rust original, because
// nothing cross-checks the two implementations against each other, are: the exact-match
// degenerate case, the leftmost-occurrence argument in the loop below, and the four
// characters [isLineTerminator] counts.
func GlobMatches(pattern, value string) bool {
	first, rest, found := strings.Cut(pattern, "*")
	if !found {
		return value == pattern
	}

	remaining, ok := strings.CutPrefix(value, first)
	if !ok {
		return false
	}

	last := rest
	if i := strings.LastIndex(rest, "*"); i >= 0 {
		between, tail := rest[:i], rest[i+1:]
		for _, segment := range strings.Split(between, "*") {
			if segment == "" {
				// Adjacent `*`s, or a run bordering the one already peeled off above —
				// either way, an empty literal is found at the current position for free,
				// with no gap to check.
				continue
			}
			gap, after, found := strings.Cut(remaining, segment)
			if !found {
				return false
			}
			if strings.ContainsFunc(gap, isLineTerminator) {
				// The leftmost occurrence is the only one worth trying: its gap is a prefix
				// of every later occurrence's gap, so if this one already crosses a line
				// terminator, every later one crosses it too, and there is nothing left to
				// search for.
				return false
			}
			remaining = after
		}
		last = tail
	}

	gap, ok := strings.CutSuffix(remaining, last)
	if !ok {
		return false
	}
	return !strings.ContainsFunc(gap, isLineTerminator)
}
