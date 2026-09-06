#!/usr/bin/env bash
#
# Re-run the #195 taint-analysis calibration against an external corpus checkout.
#
# Extracts an IMMUTABLE snapshot of the pinned corpus commit (git archive, never a working
# checkout — AGENTS.md discipline), drops the committed calibration rule + config into its
# root, and runs lanekeep over it, emitting the run's JSON. The rule/config are transcribed
# from docs/taint-calibration.md's appendix; the pinned SHA is the corpus #195 measured.
#
# Usage:
#   scripts/taint-calibration.sh <corpus-repo> [<out.json>]
#
# Env overrides (for testing; the defaults are the published measurement's):
#   CALIB_CORPUS_SHA   the corpus commit to archive (default: the #195 SHA)
#   LANEKEEP_BIN       the lanekeep binary to run (default: lanekeep on PATH)

set -euo pipefail

CORPUS_SHA="${CALIB_CORPUS_SHA:-3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd}"
LANEKEEP_BIN="${LANEKEEP_BIN:-lanekeep}"

here="$(cd "$(dirname "$0")" && pwd)"
calib_dir="$here/calibration"

usage() {
    echo "usage: $0 <corpus-repo> [<out.json>]" >&2
    exit 2
}

[ $# -ge 1 ] || usage
corpus="$1"
out="${2:-}"

[ -d "$corpus" ] || { echo "error: corpus repo not found: $corpus" >&2; exit 2; }

# git with the worktree-poisoning variables cleared: a hook exports GIT_DIR, and then
# `git -C <corpus>` would target the hook's repository rather than the corpus (AGENTS.md).
git_() { env -u GIT_DIR -u GIT_WORK_TREE git "$@"; }

# The corpus must carry the exact pinned commit, or the numbers are not comparable.
if [ "$(git_ -C "$corpus" cat-file -t "$CORPUS_SHA" 2>/dev/null || true)" != "commit" ]; then
    echo "error: corpus '$corpus' has no commit $CORPUS_SHA" >&2
    echo "       fetch it first, or the calibration is not comparable to the published figures." >&2
    exit 1
fi

# The binary must exist, whether named on PATH or given as a path.
if ! command -v "$LANEKEEP_BIN" >/dev/null 2>&1 && [ ! -x "$LANEKEEP_BIN" ]; then
    echo "error: lanekeep binary not found: $LANEKEEP_BIN" >&2
    echo "       build it: cargo build --release -p lanekeep-cli (then LANEKEEP_BIN=target/release/lanekeep)" >&2
    exit 1
fi

snapshot="$(mktemp -d)"
trap 'rm -rf "$snapshot"' EXIT

# An immutable extract, NOT a working checkout.
git_ -C "$corpus" archive "$CORPUS_SHA" | tar -x -C "$snapshot"

# Both must sit at the check root: rule-module resolution is confined to it.
cp "$calib_dir/lanekeep.json" "$calib_dir/no-secret-in-string.calib.ts" "$snapshot/"

# lanekeep exits 0 (clean), 1 (violations found), or 2 (run aborted / limit breach).
# Findings are the expected case here — only an abort is a harness failure.
rc=0
json="$("$LANEKEEP_BIN" check "$snapshot" --no-cache --format json)" || rc=$?
if [ "$rc" -ge 2 ]; then
    echo "error: lanekeep exited $rc — a run abort or limit breach, not findings" >&2
    exit "$rc"
fi

# Pull files_parsed out of the payload without a JSON tool (bash only). The field name is
# stable; the value is the digits immediately after it, whether pretty-printed or compact.
files_parsed="$(printf '%s' "$json" | sed -n 's/.*"files_parsed"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -1)"
if [ -z "$files_parsed" ] || [ "$files_parsed" -eq 0 ] 2>/dev/null; then
    echo "error: lanekeep parsed 0 files — the include globs matched nothing in the snapshot" >&2
    exit 1
fi

total="$(printf '%s' "$json" | sed -n 's/.*"total"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -1)"
echo "calibration: $files_parsed files parsed, ${total:-?} findings (corpus $CORPUS_SHA)" >&2

if [ -n "$out" ]; then
    printf '%s\n' "$json" > "$out"
    echo "wrote $out" >&2
else
    printf '%s\n' "$json"
fi
