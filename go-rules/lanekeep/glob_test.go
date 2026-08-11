package lanekeep

import "testing"

// The cases are the ones `rust-rules/lanekeep-rule/src/lib.rs`'s own tests cover, carried
// over one for one so the two SDKs can be compared by reading. The `group` field names the
// Rust test each row came from; a row that fails should send you to the same reasoning
// there.
func TestGlobMatches(t *testing.T) {
	cases := []struct {
		group   string
		pattern string
		value   string
		want    bool
	}{
		// a_pattern_is_anchored_at_both_ends
		{"anchored", "super", "super::*", false},
		{"anchored", "super::*", "super::*", true},
		// The no-`*` rows above are decided by a plain equality and prove nothing about a
		// pattern that actually contains a `*`, which is every pattern the built-in rules'
		// `allow` lists use. Both ends, checked independently: a value with an extra
		// trailing byte after where the pattern's tail should land, and one with an extra
		// leading byte before where its head should start.
		{"anchored", "subject/*.rs", "subject/input.rs.bak", false},
		{"anchored", "subject/*.rs", "xsubject/input.rs", false},

		// a_star_spans_a_path_segment
		{"spans a segment", "subject/*.rs", "subject/input.rs", true},

		// a_star_does_not_span_a_line_terminator.
		//
		// The TypeScript original these were ported from builds a `RegExp` carrying no
		// `s`/`dotAll` flag, so `.` — and by extension `*` — does not match a line
		// terminator. Reachable: `no-glob-import` defaults `allow` to `['*prelude*']` and
		// reports at the wildcard's text, whose node can legitimately wrap onto a second
		// line. `\r\n` closes the same gap on Windows line endings.
		{"line terminator", "*prelude*", "std::\n    prelude::*", false},
		{"line terminator", "*prelude*", "std::\r\n    prelude::*", false},
		{"line terminator", "*prelude*", "std::prelude::*", true},
		// ECMAScript counts four characters as line terminators, and the two beyond `\n`
		// and `\r` are multi-byte. Go's scan has to see them as runes, not as the bytes
		// they decode to. Written as escapes rather than literally, because a raw one is
		// invisible in an editor — AGENTS.md records a NUL in a rule source reporting a
		// parse failure twenty lines from where it sat.
		{"line terminator", "*prelude*", "std::\u2028prelude::*", false},
		{"line terminator", "*prelude*", "std::\u2029prelude::*", false},

		// a_regex_metacharacter_in_a_pattern_is_a_literal. The TypeScript original escapes
		// these before building a RegExp. An implementation that forgot would make `a.rs`
		// match `axrs`.
		{"metacharacter is literal", "a.rs", "axrs", false},
		{"metacharacter is literal", "a.rs", "a.rs", true},
	}

	for _, c := range cases {
		if got := GlobMatches(c.pattern, c.value); got != c.want {
			t.Errorf("%s: GlobMatches(%q, %q) = %v, want %v", c.group, c.pattern, c.value, got, c.want)
		}
	}
}
