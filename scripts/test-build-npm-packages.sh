#!/usr/bin/env bash
# Tests for build-npm-packages.sh and the npm launcher's platform resolution.
#
# The packaging script runs once per release, on a machine nobody is watching, and its
# failure mode is a published package that installs and then cannot run. That is worth
# testing here rather than discovering from a bug report.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/build-npm-packages.sh"

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
trap 'rm -rf "${work}"; rm -rf "${repo_root}/npm/@lanekeep"' EXIT

# --- a complete set of binaries packages cleanly ---------------------------------------
artifacts="${work}/dist"
for triple in aarch64-apple-darwin x86_64-apple-darwin \
  aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir -p "${artifacts}/${triple}"
  printf 'binary' >"${artifacts}/${triple}/lanekeep"
done
mkdir -p "${artifacts}/x86_64-pc-windows-msvc"
printf 'binary' >"${artifacts}/x86_64-pc-windows-msvc/lanekeep.exe"

"${script}" 9.9.9 "${artifacts}" >/dev/null 2>&1
check "a complete set packages" "0" "$?"

check "the launcher takes the version" "9.9.9" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "${repo_root}/npm/lanekeep/package.json")"

check "a platform package takes the version" "9.9.9" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "${repo_root}/npm/@lanekeep/darwin-arm64/package.json")"

# Every optional dependency must be pinned to this release. A launcher pointing at last
# release's binaries installs and then runs the wrong version.
check "optional dependencies are pinned to the same version" "1" \
  "$(python3 -c '
import json, sys
package = json.load(open(sys.argv[1]))
print(len(set(package["optionalDependencies"].values())))' \
    "${repo_root}/npm/lanekeep/package.json")"

check "a platform package declares its os" "darwin" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["os"][0])' \
    "${repo_root}/npm/@lanekeep/darwin-arm64/package.json")"

check "a platform package declares its cpu" "arm64" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["cpu"][0])' \
    "${repo_root}/npm/@lanekeep/darwin-arm64/package.json")"

check "the windows binary keeps its extension" "0" \
  "$([ -f "${repo_root}/npm/@lanekeep/win32-x64/bin/lanekeep.exe" ] && echo 0 || echo 1)"

check "a unix binary is executable" "0" \
  "$([ -x "${repo_root}/npm/@lanekeep/linux-x64/bin/lanekeep" ] && echo 0 || echo 1)"

# --- a missing binary fails loudly ------------------------------------------------------
rm -rf "${repo_root}/npm/@lanekeep"
missing="${work}/partial"
mkdir -p "${missing}/aarch64-apple-darwin"
printf 'binary' >"${missing}/aarch64-apple-darwin/lanekeep"

"${script}" 9.9.9 "${missing}" >/dev/null 2>&1
check "a missing platform binary fails the build" "1" "$?"

# --- the launcher resolves platforms ------------------------------------------------------
if command -v node >/dev/null 2>&1; then
  resolve="${repo_root}/npm/lanekeep/resolve.js"

  check "every declared platform has a package name" "0" \
    "$(node -e '
const { PACKAGES } = require(process.argv[1]);
const declared = Object.keys(require(process.argv[2]).optionalDependencies).sort();
const known = Object.values(PACKAGES).sort();
process.exit(JSON.stringify(declared) === JSON.stringify(known) ? 0 : 1);
' "${resolve}" "${repo_root}/npm/lanekeep/package.json"; echo $?)"

  check "an unsupported platform names what is available" "0" \
    "$(node -e '
const { resolveBinary } = require(process.argv[1]);
try {
  resolveBinary("sunos", "sparc");
  process.exit(1);
} catch (error) {
  // It must say what *is* available: "cannot find module @lanekeep/sunos-sparc" tells a
  // user nothing they can act on.
  process.exit(error.message.includes("available:") ? 0 : 1);
}
' "${resolve}"; echo $?)"

  check "a missing package says how to fix it" "0" \
    "$(node -e '
const { resolveBinary } = require(process.argv[1]);
try {
  // A platform lanekeep ships for, whose package is not installed here.
  resolveBinary("linux", "arm64");
  process.exit(1);
} catch (error) {
  process.exit(error.message.includes("reinstall") ? 0 : 1);
}
' "${resolve}"; echo $?)"

  check "windows gets the .exe name" "lanekeep.exe" \
    "$(node -e '
const { binaryName } = require(process.argv[1]);
process.stdout.write(binaryName("win32"));
' "${resolve}")"

  check "unix gets the plain name" "lanekeep" \
    "$(node -e '
const { binaryName } = require(process.argv[1]);
process.stdout.write(binaryName("linux"));
' "${resolve}")"
else
  echo "note: node not found, skipping launcher tests"
fi

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
