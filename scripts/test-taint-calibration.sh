#!/usr/bin/env bash
#
# Tests for taint-calibration.sh — its orchestration, not lanekeep's detection. lanekeep is
# stubbed and the corpus is a throwaway git repo, so this runs in the gate (just test-scripts)
# with no external corpus and no release build. The detection is proven separately by
# crates/lanekeep-rules/tests/calibration_queries.rs and by the real run recorded in
# docs/taint-calibration.md.

set -uo pipefail

cd "$(dirname "$0")/.."
HARNESS="./scripts/taint-calibration.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# git without the worktree-poisoning variables a hook exports (AGENTS.md core.bare trap):
# `git init` under an exported GIT_DIR initializes the real repository as bare.
git_() { env -u GIT_DIR -u GIT_WORK_TREE git "$@"; }

# --- a throwaway corpus repo carrying one commit ------------------------------
corpus="$work/corpus"
mkdir -p "$corpus/apps/x/src"
echo "export const a = 1" > "$corpus/apps/x/src/a.ts"
git_ -C "$corpus" init -q
git_ -C "$corpus" config user.email t@example.com
git_ -C "$corpus" config user.name tester
git_ -C "$corpus" add -A
git_ -C "$corpus" commit -q -m "seed"
corpus_sha="$(git_ -C "$corpus" rev-parse HEAD)"

# --- a stubbed lanekeep that records its argv and prints a known JSON payload -
stub="$work/lanekeep"
argv_log="$work/argv"
cat > "$stub" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" > "$argv_log"
cat <<'JSON'
{
  "version": 1,
  "ok": true,
  "total": 2,
  "files_discovered": 5,
  "files_parsed": 5,
  "warn_only": false,
  "violations": []
}
JSON
EOF
chmod +x "$stub"

pass=0
fail=0
ok() { pass=$((pass + 1)); }
no() { fail=$((fail + 1)); printf '  \033[31m✖\033[0m %s\n' "$1"; }

# 1. happy path: correct SHA + stub binary → exit 0, and out.json carries the stub payload.
out="$work/out.json"
if CALIB_CORPUS_SHA="$corpus_sha" LANEKEEP_BIN="$stub" "$HARNESS" "$corpus" "$out" >/dev/null 2>&1; then ok; else no "happy path should exit 0"; fi
grep -q '"files_parsed": 5' "$out" 2>/dev/null && ok || no "out.json should carry the stub payload"

# 2. lanekeep was invoked with --no-cache and --format json.
grep -q -- "--no-cache" "$argv_log" 2>/dev/null && ok || no "harness must pass --no-cache"
grep -q -- "--format json" "$argv_log" 2>/dev/null && ok || no "harness must pass --format json"

# 3. wrong SHA → non-zero (the corpus lacks the pinned commit).
if CALIB_CORPUS_SHA="0000000000000000000000000000000000000000" LANEKEEP_BIN="$stub" "$HARNESS" "$corpus" "$out" >/dev/null 2>&1; then no "wrong SHA should fail"; else ok; fi

# 4. files_parsed == 0 → non-zero (the include globs matched nothing).
stub0="$work/lanekeep0"
cat > "$stub0" <<'EOF'
#!/usr/bin/env bash
cat <<'JSON'
{ "version": 1, "ok": true, "total": 0, "files_discovered": 0, "files_parsed": 0, "warn_only": false, "violations": [] }
JSON
EOF
chmod +x "$stub0"
if CALIB_CORPUS_SHA="$corpus_sha" LANEKEEP_BIN="$stub0" "$HARNESS" "$corpus" "$out" >/dev/null 2>&1; then no "files_parsed==0 should fail"; else ok; fi

# 5. missing binary → non-zero.
if CALIB_CORPUS_SHA="$corpus_sha" LANEKEEP_BIN="$work/nonexistent" "$HARNESS" "$corpus" "$out" >/dev/null 2>&1; then no "missing binary should fail"; else ok; fi

# 6. missing corpus directory → non-zero.
if LANEKEEP_BIN="$stub" "$HARNESS" "$work/nodir" "$out" >/dev/null 2>&1; then no "missing corpus should fail"; else ok; fi

# 7. lanekeep exits 1 when it finds violations (linter convention: 0 clean, 1 has
#    error-severity violations, 2 run aborted/limit breach — lanekeep_report::exit_code).
#    The harness runs the check as `json="$(lanekeep ...)"` under `set -euo pipefail`, so an
#    unguarded command substitution aborts the whole script the instant a real run has any
#    findings, with no output at all. The stub above always exits 0, which is exactly why
#    this case did not exist before. Findings are the expected case here: the harness must
#    still exit 0 and write the payload.
stub1="$work/lanekeep1"
cat > "$stub1" <<'EOF'
#!/usr/bin/env bash
cat <<'JSON'
{ "version": 1, "ok": false, "total": 1, "files_discovered": 5, "files_parsed": 5, "warn_only": false, "violations": [{"ruleId": "x", "file": "a.ts", "line": 1, "column": 1, "message": "m", "severity": "error"}] }
JSON
exit 1
EOF
chmod +x "$stub1"
out1="$work/out1.json"
if CALIB_CORPUS_SHA="$corpus_sha" LANEKEEP_BIN="$stub1" "$HARNESS" "$corpus" "$out1" >/dev/null 2>&1; then ok; else no "findings (exit 1) should still exit 0"; fi
grep -q '"files_parsed": 5' "$out1" 2>/dev/null && ok || no "out.json should carry the findings payload"

# 8. lanekeep exits 2 (run aborted / limit breach) → non-zero, never swallowed as a finding.
stub2="$work/lanekeep2"
cat > "$stub2" <<'EOF'
#!/usr/bin/env bash
echo "run aborted: timeout" >&2
exit 2
EOF
chmod +x "$stub2"
if CALIB_CORPUS_SHA="$corpus_sha" LANEKEEP_BIN="$stub2" "$HARNESS" "$corpus" "$out" >/dev/null 2>&1; then no "abort (exit 2) should fail"; else ok; fi

printf 'taint-calibration: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
