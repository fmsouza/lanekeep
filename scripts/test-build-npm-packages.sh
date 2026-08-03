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
# Only the temporary directory. The script writes nowhere else, which is the property
# this test would otherwise be quietly violating — an earlier version rewrote the tracked
# launcher manifest and left a release version committed.
trap 'rm -rf "${work}"' EXIT

out="${work}/out"

# --- a complete set of binaries packages cleanly ---------------------------------------
artifacts="${work}/dist"
for triple in aarch64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir -p "${artifacts}/${triple}"
  printf 'binary' >"${artifacts}/${triple}/lanekeep"
done
mkdir -p "${artifacts}/x86_64-pc-windows-msvc"
printf 'binary' >"${artifacts}/x86_64-pc-windows-msvc/lanekeep.exe"

"${script}" 9.9.9 "${artifacts}" "${out}" >/dev/null 2>&1
check "a complete set packages" "0" "$?"

check "the launcher takes the version" "9.9.9" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "${out}/lanekeep/package.json")"

check "a platform package takes the version" "9.9.9" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "${out}/@lanekeep/darwin-arm64/package.json")"

# Every optional dependency must be pinned to this release. A launcher pointing at last
# release's binaries installs and then runs the wrong version.
check "optional dependencies are pinned to the same version" "1" \
  "$(python3 -c '
import json, sys
package = json.load(open(sys.argv[1]))
print(len(set(package["optionalDependencies"].values())))' \
    "${out}/lanekeep/package.json")"

check "a platform package declares its os" "darwin" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["os"][0])' \
    "${out}/@lanekeep/darwin-arm64/package.json")"

check "a platform package declares its cpu" "arm64" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["cpu"][0])' \
    "${out}/@lanekeep/darwin-arm64/package.json")"

check "the windows binary keeps its extension" "0" \
  "$([ -f "${out}/@lanekeep/win32-x64/bin/lanekeep.exe" ] && echo 0 || echo 1)"

check "a unix binary is present" "0" \
  "$([ -s "${out}/@lanekeep/linux-x64/bin/lanekeep" ] && echo 0 || echo 1)"

# The executable bit only exists where the filesystem has one. Asserting it on Windows tests
# the runner, not the script — and the packaging that matters there is the `.exe` name.
if [ "$(uname -s 2>/dev/null || echo unknown)" != "unknown" ] &&
  ! uname -s | grep -qiE 'mingw|msys|cygwin'; then
  check "a unix binary is executable" "0" \
    "$([ -x "${out}/@lanekeep/linux-x64/bin/lanekeep" ] && echo 0 || echo 1)"
fi

# --- the authoring types ship with the launcher -----------------------------------------------
#
# `lanekeep` is the name a rule imports from, so the types have to live under that name for an
# editor to find them. Shipping them anywhere else would give autocomplete to nobody.
#
# Placed here rather than at the end of the file on purpose: the negative test below empties
# `${out}` deliberately, and checks written after it inspect a directory that is gone. Which
# they did, at first — every one of them "failed" against nothing.
for file in index.js index.d.ts builtin.d.ts; do
  check "the launcher ships ${file}" "1" \
    "$([ -f "${out}/lanekeep/${file}" ] && echo 1 || echo 0)"
done

check "package.json points at the types" "index.d.ts" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("types",""))' \
    "${out}/lanekeep/package.json")"

# npm publishes only what `files` lists. A definition present in the directory and absent from
# that list never reaches anyone, and nothing else would say so — the package installs fine and
# the editor simply stays quiet.
#
# Read from the parsed array rather than grepped: every one of these names also appears in
# `types` or `typesVersions`, so a substring count answers two and proves nothing.
check "the types are in the published file list" "" \
  "$(python3 -c '
import json, sys
listed = set(json.load(open(sys.argv[1])).get("files", []))
print(", ".join(sorted({"index.js", "index.d.ts", "builtin.d.ts"} - listed)))
' "${out}/lanekeep/package.json")"

# Every specifier resolves through builtin.d.ts, the bare one included, so it has to re-export
# index — otherwise `import { defineRule } from 'lanekeep'` lands on a file without it. That is
# exactly how it failed before a compile test caught it.
check "the subpath types re-export the main ones" "1" \
  "$(grep -c "export \* from './index'" "${out}/lanekeep/builtin.d.ts" | tr -d ' ')"

# --- a missing binary fails loudly ------------------------------------------------------
rm -rf "${out}"
missing="${work}/partial"
mkdir -p "${missing}/aarch64-apple-darwin"
printf 'binary' >"${missing}/aarch64-apple-darwin/lanekeep"

"${script}" 9.9.9 "${missing}" "${out}" >/dev/null 2>&1
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

# The template is not build output and must not carry a release version.
check "the committed launcher keeps its placeholder version" "0.0.0" \
  "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "${repo_root}/npm/lanekeep/package.json")"

check "the script writes nothing into npm/" "0" \
  "$([ ! -d "${repo_root}/npm/@lanekeep" ] && echo 0 || echo 1)"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
