#!/usr/bin/env bash
#
# Tests for lint-commit-msg.sh.
#
# This script gates every commit and every pull request title, and a pull request
# title is what release-plz reads to decide the next version. A false accept here
# produces a wrong release; a false reject blocks everyone. It gets tests.

set -uo pipefail

cd "$(dirname "$0")/.."
LINTER="./scripts/lint-commit-msg.sh"

pass=0
fail=0

accepts() {
    if "$LINTER" --message "$1" >/dev/null 2>&1; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf '  \033[31m✖\033[0m should accept but rejected: %s\n' "$1"
    fi
}

rejects() {
    if "$LINTER" --message "$1" >/dev/null 2>&1; then
        fail=$((fail + 1))
        printf '  \033[31m✖\033[0m should reject but accepted: %s\n' "$1"
    else
        pass=$((pass + 1))
    fi
}

# --- valid conventional commits ------------------------------------------------
accepts "feat(core): add violation and rule card types"
accepts "fix: include tracked reads in the entry key"
accepts "docs: explain why breaching a timeout cancels the run"
accepts "chore(lang-js): bump tree-sitter grammars"
accepts "ci: add security workflow"
accepts "perf: reduce boundary crossings"
accepts "test(js): assert the sandbox withholds the clock"
accepts "build: pin actions to commit shas"
accepts "style: reflow doc comments"
accepts "refactor(cache): extract the index reader"
accepts "revert: feat(core): add the thing"

# --- breaking change marker ----------------------------------------------------
accepts "feat(js)!: replace the node handle representation"
accepts "feat!: rename the config file"

# --- scopes with punctuation ---------------------------------------------------
accepts "fix(lang-js): handle tsx fragments"
accepts "fix(host.api): correct the arity check"
accepts "fix(js/loader): reject bare specifiers"

# --- git's own messages, which must pass through untouched ---------------------
accepts "Merge branch 'main' into feat/host-api"
accepts "Merge pull request #12 from fmsouza/topic"
accepts "fixup! feat: something"
accepts "squash! fix: something"
accepts "amend! docs: something"

# --- acronyms and identifiers may legitimately be capitalized ------------------
accepts "feat: add QuickJS interrupt handler"
accepts "fix: correct SARIF severity mapping"

# --- malformed ------------------------------------------------------------------
rejects "added a thing"
rejects "feat add a thing"
rejects "feat(core) add a thing"
rejects ""
rejects "feat:"
rejects "feat: "

# --- wrong or invented types ----------------------------------------------------
rejects "wip: something"
rejects "feature: add a thing"
rejects "bugfix: correct a thing"
rejects "FEAT: add a thing"

# --- style rules ------------------------------------------------------------------
rejects "feat(core): Add violation types"
rejects "feat(core): add violation types."
rejects "feat(core): this subject line is deliberately far too long to be acceptable in a git history"

# --- the boundary of the length rule ----------------------------------------------
# 72 characters exactly, which is the limit and must pass.
accepts "feat(core): aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
# 73 characters, one over.
rejects "feat(core): aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

# --- reading from a file, the way the commit-msg hook calls it ---------------------
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

printf 'feat(core): add the thing\n\nA longer body explaining why.\n' > "$tmp"
if "$LINTER" "$tmp" >/dev/null 2>&1; then
    pass=$((pass + 1))
else
    fail=$((fail + 1))
    printf '  \033[31m✖\033[0m should accept a well-formed message file\n'
fi

# Comment lines and leading blanks are what git actually hands the hook.
printf '\n# Please enter the commit message for your changes.\n#\nfeat: add the thing\n' > "$tmp"
if "$LINTER" "$tmp" >/dev/null 2>&1; then
    pass=$((pass + 1))
else
    fail=$((fail + 1))
    printf '  \033[31m✖\033[0m should skip comments and blanks to find the subject\n'
fi

printf '# only a comment\n' > "$tmp"
if "$LINTER" "$tmp" >/dev/null 2>&1; then
    fail=$((fail + 1))
    printf '  \033[31m✖\033[0m should reject a message that is only comments\n'
else
    pass=$((pass + 1))
fi

# --- report -------------------------------------------------------------------------
echo
if [ "$fail" -eq 0 ]; then
    printf '\033[32m✔\033[0m lint-commit-msg: %d assertions passed\n' "$pass"
    exit 0
fi
printf '\033[31m✖\033[0m lint-commit-msg: %d passed, %d failed\n' "$pass" "$fail"
exit 1
