#!/usr/bin/env bash
# Tests for publish-npm.sh.
#
# This script runs once per release, holding a registry token, against a registry that will
# not let it take anything back. Both bugs it guards against shipped in v0.1.0 and were only
# visible from the failed run's log, so they are worth pinning here where they cost nothing
# to check.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/publish-npm.sh"

passed=0
failed=0

check() {
  local name="$1" expected="$2" actual="$3"
  if [ "${expected}" = "${actual}" ]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL ${name}"
    echo "  expected: ${expected}"
    echo "  actual:   ${actual}"
  fi
}

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# A stand-in for npm: it records what it was asked to do and answers `view` from a list of
# versions the registry is pretending to already have.
cat >"${work}/npm" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${NPM_LOG}"
if [ "$1" = "view" ]; then
  grep -qxF "$2" "${NPM_PUBLISHED}" 2>/dev/null && exit 0
  exit 1
fi
if [ "$1" = "publish" ]; then
  # npm packs whatever mode it finds, so record the mode this publish would have shipped.
  for file in "$2"/bin/*; do
    [ -f "${file}" ] || continue
    if [ -x "${file}" ]; then
      printf 'mode exec %s\n' "${file}" >>"${NPM_LOG}"
    else
      printf 'mode noexec %s\n' "${file}" >>"${NPM_LOG}"
    fi
  done
fi
exit 0
STUB
chmod +x "${work}/npm"

export NPM="${work}/npm"
export NPM_LOG="${work}/log"
export NPM_PUBLISHED="${work}/published"

# Relative, and run from a directory that makes it so. This is not incidental: `dist-npm/lanekeep`
# is the exact spelling npm mistakes for a GitHub shorthand, and an absolute path would sidestep
# the bug rather than test it. CI passes a relative path too.
cd "${work}" || exit 1
dist="dist-npm"

# The version the workspace manifest declares. The fixtures use it rather than a literal so
# they keep matching after a release bump — publish-npm.sh refuses to publish a version that
# disagrees with Cargo.toml, and a hardcoded fixture would turn every bump into a test failure.
workspace_version="$(python3 -c '
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r"^version\s*=\s*\"([^\"]+)\"", section, re.MULTILINE).group(1))
' "${repo_root}/Cargo.toml")"

reset() {
  local version="${1:-${workspace_version}}"
  rm -rf "${dist}"
  : >"${NPM_LOG}"
  : >"${NPM_PUBLISHED}"
  for name in darwin-arm64 linux-arm64 linux-x64 win32-x64; do
    mkdir -p "${dist}/@lanekeep/${name}/bin"
    printf '{"name":"@lanekeep/%s","version":"%s"}\n' "${name}" "${version}" \
      >"${dist}/@lanekeep/${name}/package.json"
    # Deliberately not executable. This is the state `download-artifact` hands over — it
    # unzips without permissions — and reproducing it is the whole point of the fixture.
    printf 'binary' >"${dist}/@lanekeep/${name}/bin/lanekeep"
    chmod 644 "${dist}/@lanekeep/${name}/bin/lanekeep"
  done
  mkdir -p "${dist}/lanekeep/bin"
  printf '{"name":"lanekeep","version":"%s"}\n' "${version}" \
    >"${dist}/lanekeep/package.json"
  printf '#!/usr/bin/env node\n' >"${dist}/lanekeep/bin/lanekeep"
  chmod 644 "${dist}/lanekeep/bin/lanekeep"
}

# The paths handed to `npm publish`, in order.
published_paths() {
  grep '^publish ' "${NPM_LOG}" | awk '{print $2}'
}

# --- a clean registry publishes everything -----------------------------------------------
reset
"${script}" "${dist}" >/dev/null 2>&1
check "a clean run succeeds" "0" "$?"
check "every package is published" "5" "$(published_paths | wc -l | tr -d ' ')"

# --- the bug that broke v0.1.0 -------------------------------------------------------------
#
# `npm publish dist-npm/lanekeep` is not a directory publish. npm reads a bare `<a>/<b>` as a
# GitHub shorthand and tries to clone it over SSH. Every path must be unmistakably a path.
check "no path can be read as a git shorthand" "0" \
  "$(published_paths | grep -cvE '^(\./|/)' | tr -d ' ')"

check "the launcher specifically is a path" "1" \
  "$(published_paths | grep -cE '^\./.*/lanekeep$' | tr -d ' ')"

# --- the binary ships executable --------------------------------------------------------------
#
# v0.1.0 published a binary at 0644 and every invocation of it died with EACCES. npm packs the
# mode it finds and infers nothing from `bin`, and the `chmod +x` applied at packaging time is
# undone by the artifact round trip between the package and publish jobs. Nothing between those
# two points can see the problem, so it is asserted here, at the moment npm would pack.
#
# The executable bit only exists where the filesystem has one; asserting it elsewhere tests the
# runner rather than the script.
if [ "$(uname -s 2>/dev/null || echo unknown)" != "unknown" ] &&
  ! uname -s | grep -qiE 'mingw|msys|cygwin'; then
  check "nothing is published without the executable bit" "0" \
    "$(grep -c '^mode noexec ' "${NPM_LOG}" | tr -d ' ')"
  check "every published binary is executable" "5" \
    "$(grep -c '^mode exec ' "${NPM_LOG}" | tr -d ' ')"
fi

# --- ordering ------------------------------------------------------------------------------
#
# The launcher declares the platform packages as optional dependencies. Publishing it first
# leaves a window where `npm install lanekeep` resolves binaries that do not exist.
check "the launcher publishes last" "0" \
  "$(published_paths | tail -1 | grep -cvE '/lanekeep$' | tr -d ' ')"

# --- an already-published version is skipped ------------------------------------------------
reset
for name in darwin-arm64 linux-arm64 linux-x64 win32-x64; do
  echo "@lanekeep/${name}@${workspace_version}" >>"${NPM_PUBLISHED}"
done
echo "lanekeep@${workspace_version}" >>"${NPM_PUBLISHED}"
"${script}" "${dist}" >/dev/null 2>&1
check "a fully published release succeeds" "0" "$?"
check "and publishes nothing again" "0" "$(published_paths | wc -l | tr -d ' ')"

# --- the state v0.1.0 actually got stuck in --------------------------------------------------
#
# Four platform packages up, launcher not. Without the skip, the re-run dies on the first
# platform package and the launcher is unreachable at this version forever.
reset
for name in darwin-arm64 linux-arm64 linux-x64 win32-x64; do
  echo "@lanekeep/${name}@${workspace_version}" >>"${NPM_PUBLISHED}"
done
"${script}" "${dist}" >/dev/null 2>&1
check "a partial release resumes" "0" "$?"
check "and publishes only what is missing" "1" "$(published_paths | wc -l | tr -d ' ')"
check "which is the launcher" "1" \
  "$(published_paths | grep -cE '/lanekeep$' | tr -d ' ')"

# --- a launcher with no binaries beside it is refused -------------------------------------
reset
rm -rf "${dist:?}/@lanekeep"
"${script}" "${dist}" >/dev/null 2>&1
check "no platform packages fails the publish" "1" "$?"
check "and the launcher is not published alone" "0" \
  "$(published_paths | wc -l | tr -d ' ')"

# --- a version the manifest disagrees with is refused ------------------------------------------
#
# npm learns the version from the tag and crates.io from Cargo.toml. A mismatch ships one
# release under two numbers, and neither registry lets a number be reused, so this has to fail
# before anything is published rather than halfway through.
reset 9.9.9
"${script}" "${dist}" >/dev/null 2>&1
check "a version Cargo.toml disagrees with is refused" "1" "$?"
check "and nothing is published" "0" "$(published_paths | wc -l | tr -d ' ')"

# --- flags reach npm --------------------------------------------------------------------------
reset
"${script}" "${dist}" --dry-run >/dev/null 2>&1
check "--dry-run is forwarded" "5" "$(grep -c -- '--dry-run' "${NPM_LOG}" | tr -d ' ')"

reset
"${script}" "${dist}" --nonsense >/dev/null 2>&1
check "an unknown flag is refused" "2" "$?"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
