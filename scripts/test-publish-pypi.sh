#!/usr/bin/env bash
# Tests for publish-pypi.sh.
#
# Driven entirely through stubs: no network, and nothing here can upload anything. The
# behavior under test is which wheels the script decides to send and which it leaves alone,
# which is exactly the decision that cannot be taken back once it is wrong.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/publish-pypi.sh"

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

# Stripped of carriage returns: Python's text-mode stdout writes CRLF on Windows, and this
# value ends up inside wheel filenames that are then compared as strings.
version="$(python3 - "${repo_root}/Cargo.toml" <<'PY' | tr -d '\r'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1))
PY
)"

tags="macosx_11_0_arm64 manylinux_2_17_x86_64.manylinux2014_x86_64 win_amd64"

# --- stubs ------------------------------------------------------------------------------------
#
# `twine` records every invocation instead of performing one. `curl` prints whatever the test
# has decided the index currently holds.
bin="${work}/bin"
mkdir -p "${bin}"

cat >"${bin}/twine" <<'STUB'
#!/usr/bin/env bash
echo "$@" >>"${TWINE_LOG}"
exit 0
STUB
chmod +x "${bin}/twine"

cat >"${bin}/curl" <<'STUB'
#!/usr/bin/env bash
# The real call is `curl -sf <url>`; only the canned body matters here. An empty INDEX_BODY
# stands for the 404 PyPI returns for a version it has never seen, which curl -f reports as
# a non-zero exit and an empty body.
if [ -z "${INDEX_BODY:-}" ]; then
  exit 22
fi
cat "${INDEX_BODY}"
STUB
chmod +x "${bin}/curl"

export TWINE="${bin}/twine"
export CURL="${bin}/curl"

# Build a directory of wheels. Contents are irrelevant — nothing here opens them.
make_wheels() {
  local directory="$1" wheel_version="$2"
  shift 2
  rm -rf "${directory}"
  mkdir -p "${directory}"
  local tag
  for tag in "$@"; do
    printf 'wheel' >"${directory}/lanekeep-${wheel_version}-py3-none-${tag}.whl"
  done
}

# How many wheels the upload call was actually handed. Counted as occurrences on the upload
# line rather than as matching lines: `twine check` names the same wheels a line earlier, so a
# line count answers two for any number of wheels.
uploaded() {
  grep '^upload' "${TWINE_LOG}" 2>/dev/null | grep -o 'py3-none' | wc -l | tr -d ' '
}

# An index response listing the given filenames as already present.
index_holding() {
  python3 - "$@" >"${work}/index.json" <<'PY'
import json, sys
print(json.dumps({"urls": [{"filename": name} for name in sys.argv[1:]]}))
PY
  printf '%s' "${work}/index.json"
}

dist="${work}/wheels"

# --- nothing published yet ----------------------------------------------------------------------
make_wheels "${dist}" "${version}" ${tags}
export TWINE_LOG="${work}/log-fresh"
: >"${TWINE_LOG}"
unset INDEX_BODY
output="$("${script}" "${dist}" 2>&1)"
check "a version the index has never seen publishes" "0" "$?"
check "and uploads every wheel" "3" "$(uploaded)"
check "and checks them before uploading" "1" \
  "$(grep -c '^check ' "${TWINE_LOG}" | tr -d ' ')"
check "and the upload is non-interactive" "1" \
  "$(grep -c 'upload --non-interactive' "${TWINE_LOG}" | tr -d ' ')"
# The filename check above describes the index as it was a moment ago. `--skip-existing` is
# what turns a re-run racing another one from a failed release into a no-op, so it is asserted
# rather than assumed — removing it breaks nothing that any other test here would notice.
check "and tolerates a file that appeared meanwhile" "1" \
  "$(grep -c 'upload .*--skip-existing' "${TWINE_LOG}" | tr -d ' ')"

# --- the property this script exists for -----------------------------------------------------
#
# A release that died partway leaves the *version* on the index with only some of its files.
# Asking "is this version published?" answers yes and skips exactly the work still outstanding,
# which is how a release gets stranded at a number that can never be reused. The question has
# to be asked per file.
export INDEX_BODY
INDEX_BODY="$(index_holding "lanekeep-${version}-py3-none-macosx_11_0_arm64.whl")"
export TWINE_LOG="${work}/log-partial"
: >"${TWINE_LOG}"
output="$("${script}" "${dist}" 2>&1)"
check "a half-published version resumes" "0" "$?"
check "and skips only what is already there" "1" \
  "$(printf '%s' "${output}" | grep -c 'skip.*macosx_11_0_arm64')"
check "and uploads the two that are missing" "2" "$(uploaded)"
check "leaving the published one alone" "0" \
  "$(grep -c 'macosx_11_0_arm64' "${TWINE_LOG}" | tr -d ' ')"

# --- everything already up ---------------------------------------------------------------------
INDEX_BODY="$(index_holding \
  "lanekeep-${version}-py3-none-macosx_11_0_arm64.whl" \
  "lanekeep-${version}-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl" \
  "lanekeep-${version}-py3-none-win_amd64.whl")"
export TWINE_LOG="${work}/log-complete"
: >"${TWINE_LOG}"
output="$("${script}" "${dist}" 2>&1)"
check "a fully published version is a no-op" "0" "$?"
check "and uploads nothing at all" "0" \
  "$(wc -c <"${TWINE_LOG}" | tr -d ' ')"
check "and says so" "1" \
  "$(printf '%s' "${output}" | grep -c '^pypi: every wheel .* is already published$')"

# --- the version has to agree with the workspace -----------------------------------------------
#
# The wheels learn it from the tag and crates.io learns it from Cargo.toml. If those disagree,
# one release ships under two numbers on two indexes, and neither lets a number be reused.
unset INDEX_BODY
make_wheels "${work}/wrong" "99.99.99" ${tags}
export TWINE_LOG="${work}/log-wrong"
: >"${TWINE_LOG}"
output="$("${script}" "${work}/wrong" 2>&1)"
check "a version disagreeing with Cargo.toml is refused" "1" "$?"
check "and nothing is uploaded" "0" "$(wc -c <"${TWINE_LOG}" | tr -d ' ')"
check "and the message names both versions" "1" \
  "$(printf '%s' "${output}" | grep -c "99.99.99.*${version}")"

# --- a directory holding two versions ------------------------------------------------------------
mixed="${work}/mixed"
make_wheels "${mixed}" "${version}" macosx_11_0_arm64
printf 'wheel' >"${mixed}/lanekeep-0.0.1-py3-none-win_amd64.whl"
export TWINE_LOG="${work}/log-mixed"
: >"${TWINE_LOG}"
"${script}" "${mixed}" >/dev/null 2>&1
check "wheels for two versions are refused" "1" "$?"
check "and nothing is uploaded" "0" "$(wc -c <"${TWINE_LOG}" | tr -d ' ')"

# --- an empty directory ---------------------------------------------------------------------------
#
# Publishing nothing successfully is the failure that hides: the release goes green and the
# Python lane simply does not exist for that version.
empty="${work}/empty"
mkdir -p "${empty}"
"${script}" "${empty}" >/dev/null 2>&1
check "no wheels at all is an error" "1" "$?"

# --- dry run ----------------------------------------------------------------------------------------
make_wheels "${dist}" "${version}" ${tags}
export TWINE_LOG="${work}/log-dry"
: >"${TWINE_LOG}"
output="$("${script}" "${dist}" --dry-run 2>&1)"
check "a dry run succeeds" "0" "$?"
check "and uploads nothing" "0" "$(grep -c '^upload' "${TWINE_LOG}" | tr -d ' ')"
check "but still validates the wheels" "1" "$(grep -c '^check ' "${TWINE_LOG}" | tr -d ' ')"

# --- unknown arguments -------------------------------------------------------------------------------
"${script}" "${dist}" --publish-everything >/dev/null 2>&1
check "an unknown argument is refused" "2" "$?"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
