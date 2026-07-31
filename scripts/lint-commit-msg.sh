#!/usr/bin/env bash
#
# Validate a message against Conventional Commits.
#
# Used in two places that must agree:
#   - the commit-msg hook, on each commit in a branch
#   - CI, on the pull request title
#
# The PR title is the one that matters most. `main` takes squash merges only, so the
# title becomes the commit message on main, and that is what release-plz reads to
# decide the next version. A branch full of perfect commits behind a malformed title
# still produces a malformed history.
#
# Usage:
#   lint-commit-msg.sh <path-to-message-file>
#   lint-commit-msg.sh --message "feat(core): add the thing"

set -euo pipefail

TYPES='feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert'
MAX_SUBJECT=72

if [ $# -lt 1 ]; then
    echo "usage: $0 <message-file> | --message <text>" >&2
    exit 2
fi

if [ "$1" = "--message" ]; then
    [ $# -ge 2 ] || { echo "error: --message needs a value" >&2; exit 2; }
    subject="$2"
else
    [ -f "$1" ] || { echo "error: no such file: $1" >&2; exit 2; }
    # First line that is neither blank nor a comment.
    subject="$(grep -v '^#' "$1" | grep -v '^[[:space:]]*$' | head -n1 || true)"
fi

# Git generates these itself; holding them to the convention would block normal
# operations for no benefit.
case "$subject" in
    "Merge "*|"Revert \""*|"fixup!"*|"squash!"*|"amend!"*)
        exit 0
        ;;
esac

fail() {
    echo "" >&2
    echo "  ✖ $1" >&2
    echo "" >&2
    echo "    got: ${subject}" >&2
    echo "" >&2
    echo "    Conventional Commits: <type>[(scope)][!]: <description>" >&2
    echo "    types: ${TYPES//|/, }" >&2
    echo "" >&2
    echo "    examples:" >&2
    echo "      feat(core): add violation and rule card types" >&2
    echo "      fix(cache): include tracked reads in the entry key" >&2
    echo "      docs: explain why breaching a timeout cancels the run" >&2
    echo "      feat(js)!: replace the node handle representation" >&2
    echo "" >&2
    exit 1
}

[ -n "$subject" ] && [ "$subject" != " " ] || fail "commit message is empty"

if ! printf '%s' "$subject" | grep -Eq "^(${TYPES})(\([a-z0-9._/-]+\))?!?: .+"; then
    if printf '%s' "$subject" | grep -Eq "^[a-zA-Z]+(\(.*\))?!?:"; then
        fail "unknown commit type"
    fi
    fail "does not match the Conventional Commits format"
fi

if [ "${#subject}" -gt "$MAX_SUBJECT" ]; then
    fail "subject is ${#subject} characters, limit is ${MAX_SUBJECT}"
fi

case "$subject" in
    *.) fail "subject should not end with a period" ;;
esac

# `feat: Add the thing` reads as a different voice from every other line in the log.
description="${subject#*: }"
first_char="${description:0:1}"
if [ "$first_char" != "$(printf '%s' "$first_char" | tr '[:upper:]' '[:lower:]')" ]; then
    # An acronym or an identifier is fine; a capitalized ordinary word is not.
    first_word="${description%% *}"
    if [ "$first_word" != "$(printf '%s' "$first_word" | tr '[:lower:]' '[:upper:]')" ]; then
        fail "description should start lowercase"
    fi
fi

exit 0
