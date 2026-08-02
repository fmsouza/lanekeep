#!/usr/bin/env bash
# Tests for build-homebrew-formula.sh.
#
# A wrong formula fails at `brew install`, on someone else's machine, after the release has
# already gone out — and neither registry nor tap lets a version be replaced. Cheap to check
# here instead.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/build-homebrew-formula.sh"

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

archives="${work}/dist-archives"
mkdir -p "${archives}"

# The three archives a release actually produces, plus the Windows zip that Homebrew has no
# use for. Checksums are recognizable rather than realistic, so a mix-up is visible.
cat >"${archives}/SHA256SUMS" <<'SUMS'
aaaa000000000000000000000000000000000000000000000000000000000000  lanekeep-9.9.9-aarch64-apple-darwin.tar.gz
bbbb000000000000000000000000000000000000000000000000000000000000  lanekeep-9.9.9-aarch64-unknown-linux-gnu.tar.gz
cccc000000000000000000000000000000000000000000000000000000000000  lanekeep-9.9.9-x86_64-unknown-linux-gnu.tar.gz
dddd000000000000000000000000000000000000000000000000000000000000  lanekeep-9.9.9-x86_64-pc-windows-msvc.zip
SUMS

formula="${work}/lanekeep.rb"
"${script}" 9.9.9 "${archives}" "${formula}" >/dev/null 2>&1
check "a complete release generates a formula" "0" "$?"

# --- the version reaches everything that needs it -------------------------------------------
check "the formula states the version" "1" \
  "$(grep -c '^  version "9.9.9"$' "${formula}" | tr -d ' ')"
check "every url carries the version" "3" \
  "$(grep -c 'download/v9.9.9/lanekeep-9.9.9-' "${formula}" | tr -d ' ')"

# --- checksums come from SHA256SUMS, and go to the right platform ----------------------------
#
# A formula built from recomputed hashes could disagree with the SHA256SUMS a user verifies
# against, and swapped ones only fail after the download.
check "the darwin arm checksum is the recorded one" "1" \
  "$(grep -c 'sha256 "aaaa0' "${formula}" | tr -d ' ')"
check "the linux arm checksum is the recorded one" "1" \
  "$(grep -c 'sha256 "bbbb0' "${formula}" | tr -d ' ')"
check "the linux intel checksum is the recorded one" "1" \
  "$(grep -c 'sha256 "cccc0' "${formula}" | tr -d ' ')"

check "the darwin url and its checksum are adjacent" "1" \
  "$(grep -A1 'aarch64-apple-darwin.tar.gz' "${formula}" | grep -c 'sha256 "aaaa0' | tr -d ' ')"
check "the linux arm url and its checksum are adjacent" "1" \
  "$(grep -A1 'aarch64-unknown-linux-gnu.tar.gz' "${formula}" | grep -c 'sha256 "bbbb0' | tr -d ' ')"
check "the linux intel url and its checksum are adjacent" "1" \
  "$(grep -A1 'x86_64-unknown-linux-gnu.tar.gz' "${formula}" | grep -c 'sha256 "cccc0' | tr -d ' ')"

# --- what it must not claim ------------------------------------------------------------------
#
# There is no prebuilt Intel macOS binary. A url for an archive that was never uploaded is a
# 404 at install time, which reads as Homebrew being broken rather than as a platform we do
# not ship.
check "no Intel macOS download is offered" "0" \
  "$(grep -c 'x86_64-apple-darwin' "${formula}" | tr -d ' ')"
check "and the formula says why" "1" \
  "$(grep -c 'cargo install lanekeep-cli' "${formula}" | tr -d ' ')"

# Homebrew has no use for the Windows zip, and offering it would be a download nobody can run.
check "the windows archive is not offered" "0" \
  "$(grep -c 'windows-msvc' "${formula}" | tr -d ' ')"
check "and its checksum does not leak in" "0" \
  "$(grep -c 'dddd0' "${formula}" | tr -d ' ')"

# --- the shape Homebrew needs -----------------------------------------------------------------
check "the class name matches the file" "1" \
  "$(grep -c '^class Lanekeep < Formula$' "${formula}" | tr -d ' ')"
check "the dual license is declared as one" "1" \
  "$(grep -c 'license any_of: \["MIT", "Apache-2.0"\]' "${formula}" | tr -d ' ')"
check "it installs the binary" "1" \
  "$(grep -c 'bin.install "lanekeep"' "${formula}" | tr -d ' ')"

# The test block is what `brew test` runs, and a formula whose test never asserts the version
# would pass against a binary from any release.
check "the test asserts the version it installed" "1" \
  "$(grep -c 'assert_match "lanekeep #{version}"' "${formula}" | tr -d ' ')"

# --- a missing archive fails loudly --------------------------------------------------------------
partial="${work}/partial"
mkdir -p "${partial}"
head -1 "${archives}/SHA256SUMS" >"${partial}/SHA256SUMS"
"${script}" 9.9.9 "${partial}" "${work}/partial.rb" >/dev/null 2>&1
check "a missing platform fails the build" "1" "$?"
check "and writes no formula" "1" \
  "$([ -f "${work}/partial.rb" ] && echo 0 || echo 1)"

# --- no checksum file at all ----------------------------------------------------------------------
empty="${work}/empty"
mkdir -p "${empty}"
"${script}" 9.9.9 "${empty}" "${work}/empty.rb" >/dev/null 2>&1
check "no SHA256SUMS fails the build" "1" "$?"

# --- Ruby parses it, where Ruby exists -------------------------------------------------------------
#
# The strongest check available without Homebrew: a formula with a syntax error fails at
# `brew install` on a user's machine, long after the release.
if command -v ruby >/dev/null 2>&1; then
  ruby -c "${formula}" >/dev/null 2>&1
  check "the formula is valid Ruby" "0" "$?"
else
  echo "note: no ruby here, so the syntax check is skipped"
fi

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
