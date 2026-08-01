#!/usr/bin/env bash
# Publish the workspace crates to crates.io in dependency order, skipping any version
# already there.
#
#   ./scripts/publish-crates.sh [--dry-run]
#
# The order is hand-written because crates.io rejects a crate whose dependencies are not yet
# on it, and a topological sort computed here would be one more thing that can disagree with
# the manifests without saying so.
#
# Skipping what is already published is what makes a failure survivable. crates.io is
# append-only: a version cannot be replaced, and yanking does not free the number. A
# twelve-crate release that dies on the seventh leaves six permanently published, and the
# only way out is forward. If a re-run started from the first crate it would hit
# "crate version already uploaded" and stop, so the release could never be completed at that
# version at all — the fix would be to burn a version number, every time.
#
# `--no-verify` is deliberately absent: a crate that cannot build from its own published
# form is a crate nobody can use.
set -euo pipefail

extra=()
for argument in "$@"; do
  case "${argument}" in
  --dry-run) extra+=(--dry-run) ;;
  *)
    echo "error: unknown argument ${argument}" >&2
    exit 2
    ;;
  esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Overridable so the tests can drive stubs. Nothing else sets either.
cargo_command="${CARGO:-cargo}"
probe="${CRATES_PROBE:-}"

# crates.io rate-limits *new* crates: a burst, then roughly one per ten minutes. Publishing a
# twelve-crate workspace for the first time trips it — v0.1.1 got five up before crates.io
# started answering 429 — and the limit is documented, temporary, and tells you to try again.
# Waiting it out beats making someone re-run the job by hand once every ten minutes.
#
# This costs nothing on later releases: the limit only applies to crates the registry has never
# seen, so once all twelve exist the first attempt always succeeds. Both are overridable so the
# tests do not sleep.
retry_interval="${CRATES_RETRY_INTERVAL:-630}"
max_attempts="${CRATES_MAX_ATTEMPTS:-8}"

version="$(python3 - "${repo_root}/Cargo.toml" <<'PY'
import re
import sys

# The workspace version, which every crate inherits with `version.workspace = true`.
text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1))
PY
)"

crates=(
  lanekeep-core
  lanekeep-lang
  lanekeep-lang-js
  lanekeep-query
  lanekeep-js
  lanekeep-cache
  lanekeep-config
  lanekeep-engine
  lanekeep-report
  lanekeep-rules
  lanekeep-testkit
  lanekeep-cli
)

# Is this exact version already on crates.io? The registry is the authority; asking it
# beats parsing whatever `cargo publish` prints when it refuses.
published() {
  if [ -n "${probe}" ]; then
    "${probe}" "$1" "$2"
    return $?
  fi

  local code
  code="$(curl -sS -o /dev/null -w '%{http_code}' \
    -H 'User-Agent: lanekeep-release (https://github.com/fmsouza/lanekeep)' \
    "https://crates.io/api/v1/crates/$1/$2")"
  [ "${code}" = "200" ]
}

# Publish one crate, waiting out a rate limit but nothing else.
#
# Only a 429 is retried. Any other failure — a crate that will not build from its published
# form, a bad token, a dependency not yet on the registry — is real and gets worse, not better,
# for being tried again ten minutes later.
publish_one() {
  local crate="$1" attempt=1 log
  log="$(mktemp)"

  while :; do
    # Streamed as it happens and captured at the same time: `cargo publish` compiles before it
    # uploads, and a step that prints nothing for minutes reads like a hang.
    if "${cargo_command}" publish -p "${crate}" ${extra[@]+"${extra[@]}"} 2>&1 | tee "${log}"; then
      rm -f "${log}"
      return 0
    fi

    if ! grep -qiE '429|too many requests|published too many' "${log}"; then
      rm -f "${log}"
      return 1
    fi

    if [ "${attempt}" -ge "${max_attempts}" ]; then
      echo "error: ${crate} still rate-limited after ${attempt} attempts" >&2
      rm -f "${log}"
      return 1
    fi

    echo "rate-limited; waiting ${retry_interval}s before retrying ${crate} (attempt ${attempt}/${max_attempts})"
    sleep "${retry_interval}"
    attempt=$((attempt + 1))
  done
}

echo "publishing lanekeep ${version} to crates.io"
for crate in "${crates[@]}"; do
  if published "${crate}" "${version}"; then
    echo "skip    ${crate} ${version} (already published)"
    continue
  fi

  echo "publish ${crate} ${version}"
  publish_one "${crate}"
done

echo "crates.io: all ${#crates[@]} crates are at ${version}"
