#!/usr/bin/env bash
# Tests for build-release-archives.sh.
#
# What this produces is what someone downloads from the releases page and runs directly, with
# no registry and no installer in between. Its failure modes are quiet in the same way the npm
# ones were: an archive that unpacks and then cannot execute, or one missing the licenses it
# is required to carry.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/build-release-archives.sh"

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

out="${work}/out"
artifacts="${work}/dist"

for triple in aarch64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir -p "${artifacts}/${triple}"
  printf 'binary' >"${artifacts}/${triple}/lanekeep"
done
mkdir -p "${artifacts}/x86_64-pc-windows-msvc"
printf 'binary' >"${artifacts}/x86_64-pc-windows-msvc/lanekeep.exe"

"${script}" 9.9.9 "${artifacts}" "${out}" >/dev/null 2>&1
check "a complete set packages" "0" "$?"

check "every platform gets an archive" "4" \
  "$(find "${out}" -maxdepth 1 \( -name '*.tar.gz' -o -name '*.zip' \) | wc -l | tr -d ' ')"

check "windows gets a zip" "0" \
  "$([ -f "${out}/lanekeep-9.9.9-x86_64-pc-windows-msvc.zip" ] && echo 0 || echo 1)"

check "unix gets a tarball" "0" \
  "$([ -f "${out}/lanekeep-9.9.9-aarch64-apple-darwin.tar.gz" ] && echo 0 || echo 1)"

# --- what is inside ---------------------------------------------------------------------------
unpacked="${work}/unpacked"
mkdir -p "${unpacked}"
tar -xzf "${out}/lanekeep-9.9.9-x86_64-unknown-linux-gnu.tar.gz" -C "${unpacked}"
inner="${unpacked}/lanekeep-9.9.9-x86_64-unknown-linux-gnu"

check "the archive holds one named directory" "0" \
  "$([ -d "${inner}" ] && echo 0 || echo 1)"

check "the binary is there" "0" "$([ -f "${inner}/lanekeep" ] && echo 0 || echo 1)"

# Both licenses, because the crate is dual-licensed and someone with only the tarball has no
# other copy of either.
check "the MIT license ships" "0" "$([ -s "${inner}/LICENSE-MIT" ] && echo 0 || echo 1)"
check "the Apache license ships" "0" "$([ -s "${inner}/LICENSE-APACHE" ] && echo 0 || echo 1)"
check "the readme ships" "0" "$([ -s "${inner}/README.md" ] && echo 0 || echo 1)"

# The bug that shipped in npm 0.1.0, in the other channel. `download-artifact` hands over 0644
# and a tarball preserves whatever mode it is given, so an archive built without restoring the
# bit unpacks into a binary that cannot run.
if [ "$(uname -s 2>/dev/null || echo unknown)" != "unknown" ] &&
  ! uname -s | grep -qiE 'mingw|msys|cygwin'; then
  check "the unpacked binary is executable" "0" \
    "$([ -x "${inner}/lanekeep" ] && echo 0 || echo 1)"
fi

# --- checksums ------------------------------------------------------------------------------------
check "a checksum file is written" "0" "$([ -s "${out}/SHA256SUMS" ] && echo 0 || echo 1)"

check "every archive is listed" "4" \
  "$(grep -cE '\.(tar\.gz|zip)$' "${out}/SHA256SUMS" | tr -d ' ')"

# Plain names, not paths — `sha256sum -c` looks for the file beside the checksum file, and a
# `./` prefix or an absolute path makes verification fail in a way that reads like corruption.
check "checksums name files without a path" "0" \
  "$(grep -cE ' +[./]|/' "${out}/SHA256SUMS" | tr -d ' ')"

# The checksums have to match the bytes actually shipped, which is the entire point of them.
check "the checksums verify" "0" \
  "$(cd "${out}" && { sha256sum -c SHA256SUMS >/dev/null 2>&1 ||
    shasum -a 256 -c SHA256SUMS >/dev/null 2>&1; } && echo 0 || echo 1)"

# --- a missing binary fails loudly -----------------------------------------------------------------
rm -rf "${out}"
partial="${work}/partial"
mkdir -p "${partial}/aarch64-apple-darwin"
printf 'binary' >"${partial}/aarch64-apple-darwin/lanekeep"

"${script}" 9.9.9 "${partial}" "${out}" >/dev/null 2>&1
check "a missing platform binary fails the build" "1" "$?"

# --- the version reaches the names -------------------------------------------------------------------
rm -rf "${out}"
"${script}" 1.2.3 "${artifacts}" "${out}" >/dev/null 2>&1
check "archives carry the version" "4" \
  "$(find "${out}" -maxdepth 1 -name 'lanekeep-1.2.3-*' | wc -l | tr -d ' ')"

# Nothing is left behind. The staging directory is an implementation detail and an archive of
# it would be a confusing thing to publish.
check "no staging directory survives" "1" \
  "$([ -d "${out}/.staging" ] && echo 0 || echo 1)"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
