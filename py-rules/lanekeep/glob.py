"""The `*`-wildcard matcher, ported from `rust-rules/lanekeep-rule`."""

# The four characters a `RegExp`'s `.` does not match without the `s`/`dotAll`
# flag, which is how the TypeScript rules this was ported from built theirs.
_LINE_TERMINATORS = "\n\r\u2028\u2029"


def glob_matches(pattern, value):
    """Whether value matches the `*`-wildcard pattern, anchored at both ends.

    A port of `glob_matches` in `rust-rules/lanekeep-rule/src/lib.rs`. Everything
    but `*` is a literal, and `*` does not span a line terminator. A pattern with
    no `*` at all has nothing to span, so "anchored at both ends" degenerates to
    an exact match.
    """
    if "*" not in pattern:
        return value == pattern

    first, rest = pattern.split("*", 1)
    if not value.startswith(first):
        return False
    remaining = value[len(first):]

    last = rest
    if "*" in rest:
        between, tail = rest.rsplit("*", 1)
        for segment in between.split("*"):
            if segment == "":
                # Adjacent `*`s, or a run bordering the one already peeled off
                # above — either way, an empty literal is found at the current
                # position for free, with no gap to check.
                continue
            idx = remaining.find(segment)
            if idx == -1:
                return False
            gap = remaining[:idx]
            if any(c in gap for c in _LINE_TERMINATORS):
                # The leftmost occurrence is the only one worth trying: its gap
                # is a prefix of every later occurrence's gap, so if this one
                # already crosses a line terminator, every later one crosses it
                # too, and there is nothing left to search for.
                return False
            remaining = remaining[idx + len(segment):]
        last = tail

    if not remaining.endswith(last):
        return False
    gap = remaining[: len(remaining) - len(last)]
    return not any(c in gap for c in _LINE_TERMINATORS)
